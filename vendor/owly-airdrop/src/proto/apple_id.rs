//! An Apple-issued AirDrop identity, when the host has one (§7): the validation record goes out
//! as `SenderRecordData` and `ReceiverRecordData` (ID-1), the intermediate certificate rides
//! along in the TLS chain with the Apple ID certificate it was issued for (ID-5), and the
//! contact-hash prefixes fill the BLE advert (BLE-2). Without one, everything stays anonymous.
//!
//! A host may be signed in to several Apple IDs, the way a Mac has one per user: each is an
//! [Account], with a receiver, a sender and a certificate of its own. [Account::global] is the one
//! the free functions here use, for a host with just one.
use std::collections::HashSet;
use std::sync::{Arc, OnceLock, RwLock};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppleIdentity {
    /// The DER CMS validation record.
    pub record: Vec<u8>,
    /// DER certificates that follow the leaf in the TLS chain.
    pub intermediates: Vec<Vec<u8>>,
    /// BLE hash prefixes: Apple ID, phone, email, second email.
    pub ble_hashes: [[u8; 2]; 4],
}

/// One Apple ID on this host: its identity, once installed, and its owner's contacts. Clones
/// share both, so a record renewed through one handle goes out through every receiver and sender
/// that holds another.
#[derive(Clone, Default)]
pub struct Account {
    inner: Arc<RwLock<Inner>>,
}

#[derive(Default)]
struct Inner {
    identity: Option<Arc<AppleIdentity>>,
    contacts: Option<Arc<HashSet<[u8; 32]>>>,
}

/// What a sender is to an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    /// One of the account's own devices.
    Own,
    /// Its record names one of the account's contacts.
    Contact,
}

impl std::fmt::Debug for Account {
    /// Says whether it's signed in, and nothing that identifies the account (SEC-5).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account").field("signed_in", &self.get().is_some()).finish()
    }
}

impl Account {
    /// An account with nothing installed yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The host's account, which the free functions in this module use.
    pub fn global() -> Account {
        static GLOBAL: OnceLock<Account> = OnceLock::new();
        GLOBAL.get_or_init(Account::new).clone()
    }

    /// Installs the identity, replacing an earlier one (a renewed record, a new sign-in).
    pub fn install(&self, identity: AppleIdentity) {
        self.write().identity = Some(Arc::new(identity));
    }

    pub fn get(&self) -> Option<Arc<AppleIdentity>> {
        self.read().identity.clone()
    }

    /// The record to send, if any.
    pub fn record(&self) -> Option<Vec<u8>> {
        self.get().map(|i| i.record.clone())
    }

    /// The altDsID of the installed record.
    pub fn alt_dsid(&self) -> Option<String> {
        self.get().and_then(|i| alt_dsid(&i.record))
    }

    /// Both handles are the same account.
    pub fn same(&self, other: &Account) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// A proven account (see verified_account) is this one: one of its own devices.
    pub fn own(&self, account: Option<&str>) -> bool {
        let ours = self.alt_dsid();
        ours.is_some() && account.map(str::to_string) == ours
    }

    /// The owner's contacts: SHA-256 hashes of their phone numbers and email addresses, in each
    /// spelling Apple might hash. Replaces the earlier set.
    pub fn set_contacts(&self, hashes: HashSet<[u8; 32]>) {
        self.write().contacts = Some(Arc::new(hashes));
    }

    /// The sender proved its account (see verified_account) and its record names one of the
    /// owner's contacts.
    pub fn contact(&self, sender_record: Option<&[u8]>, account: Option<&str>) -> bool {
        let (Some(record), Some(_)) = (sender_record, account) else { return false };
        let contacts = self.read().contacts.clone();
        contacts.is_some_and(|set| record_hashes(record).iter().any(|h| set.contains(h)))
    }

    /// What a sender with this record and proven account is to this account, if anything. Only
    /// a signed-in account knows anyone.
    pub fn relation(&self, sender_record: Option<&[u8]>, account: Option<&str>) -> Option<Relation> {
        if self.get().is_none() {
            None
        } else if self.own(account) {
            Some(Relation::Own)
        } else if self.contact(sender_record, account) {
            Some(Relation::Contact)
        } else {
            None
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }
}

/// Which of a host's accounts (in order of preference) a sender belongs to, so that one receiver
/// answers it and the others stay out of its list: the account it's a device of, else the first
/// that has it as a contact. None for anyone else, whom the host's one receiver for everyone
/// answers by name.
pub fn answerer(accounts: &[Account], sender_record: Option<&[u8]>, account: Option<&str>) -> Option<(usize, Relation)> {
    let mut contact = None;
    for (i, a) in accounts.iter().enumerate() {
        match a.relation(sender_record, account) {
            Some(Relation::Own) => return Some((i, Relation::Own)),
            Some(Relation::Contact) if contact.is_none() => contact = Some((i, Relation::Contact)),
            _ => {}
        }
    }
    contact
}

/// Which of a host's receivers (one per Apple ID, in household order) answers a sender, and how:
/// Some((i, Some(relation))) the receiver of the account it's proven a device or a contact of,
/// with its record; Some((i, None)) the receiver of the account its record is signed for, by name
/// (an iPhone proves itself to one receiver, not to each); None for anyone else, whom the host's
/// receiver for strangers answers by name. The others give no name, so the sender lists the host
/// once.
pub fn answering(household: &[Account], sender_record: Option<&[u8]>, proven: Option<&str>) -> Option<(usize, Option<Relation>)> {
    if let Some((i, relation)) = answerer(household, sender_record, proven) {
        return Some((i, Some(relation)));
    }
    let claimed = proven.map(str::to_string).or_else(|| record_account(sender_record))?;
    household.iter().position(|a| a.own(Some(&claimed))).map(|i| (i, None))
}

/// Installs the identity in [Account::global], replacing an earlier one (a new sign-in).
pub fn install(identity: AppleIdentity) -> bool {
    Account::global().install(identity);
    true
}

pub fn get() -> Option<Arc<AppleIdentity>> {
    Account::global().get()
}

/// The record [Account::global] sends, if any.
pub fn record() -> Option<Vec<u8>> {
    Account::global().record()
}

/// A validation record's altDsID: the same on every device signed in to one Apple ID. The
/// record is a CMS-signed XML plist; this reads just the one value, without checking it.
pub fn alt_dsid(record: &[u8]) -> Option<String> {
    const KEY: &[u8] = b"<key>altDsID</key>";
    let rest = &record[find(record, KEY)? + KEY.len()..];
    let open = find(rest, b"<string>")?;
    if !rest[..open].iter().all(u8::is_ascii_whitespace) {
        return None;
    }
    let value = &rest[open + 8..];
    let value = std::str::from_utf8(&value[..find(value, b"</string>")?]).ok()?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// The Apple ID a sender proved: the altDsID of its record, when Apple signed that record and the
/// sender's TLS client certificate (tls_account, from verify::tls_account) is the Apple ID
/// certificate of the same account. A copied record alone proves nothing: senders hand theirs to
/// every receiver they ask. Always None without the "verify" feature.
pub fn verified_account(sender_record: Option<&[u8]>, tls_account: Option<&str>) -> Option<String> {
    #[cfg(feature = "verify")]
    {
        let account = tls_account?;
        let signed = super::verify::record_account(sender_record?)?;
        (signed == account).then_some(signed)
    }
    #[cfg(not(feature = "verify"))]
    {
        let _ = (sender_record, tls_account);
        None
    }
}

/// A proven account (see verified_account) is [Account::global]: one of the host's own devices.
pub fn own(account: Option<&str>) -> bool {
    Account::global().own(account)
}

/// The account a sender's record is signed for (by Apple), proven on its connection or not:
/// enough for the host's other receivers to stay out of the list of a device of one of its
/// Apple IDs, never to tell it anything.
pub fn record_account(sender_record: Option<&[u8]>) -> Option<String> {
    #[cfg(feature = "verify")]
    {
        super::verify::record_account(sender_record?)
    }
    #[cfg(not(feature = "verify"))]
    {
        let _ = sender_record;
        None
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// [Account::global]'s contacts; see [Account::set_contacts].
pub fn set_contacts(hashes: HashSet<[u8; 32]>) {
    Account::global().set_contacts(hashes);
}

/// The hashes a validation record vouches for: its ValidatedEmailHashes and ValidatedPhoneHashes,
/// hex strings in its XML plist. Read without checking the signature.
pub fn record_hashes(record: &[u8]) -> Vec<[u8; 32]> {
    let mut out = Vec::new();
    for key in [&b"<key>ValidatedEmailHashes</key>"[..], b"<key>ValidatedPhoneHashes</key>"] {
        let Some(at) = find(record, key) else { continue };
        let rest = &record[at + key.len()..];
        let Some(open) = find(rest, b"<array>") else { continue };
        if !rest[..open].iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let body = &rest[open + 7..];
        let body = &body[..find(body, b"</array>").unwrap_or(body.len())];
        let Ok(body) = std::str::from_utf8(body) else { continue };
        let strings = body.split('<').filter_map(|t| t.split_once('>')).filter(|(tag, _)| *tag == "string");
        out.extend(strings.filter_map(|(_, v)| hex32(v.trim())));
    }
    out
}

/// A SHA-256 hash written as 64 hex digits.
pub fn hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

/// [Account::contact] for [Account::global].
pub fn contact(sender_record: Option<&[u8]>, account: Option<&str>) -> bool {
    Account::global().contact(sender_record, account)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(account: &str, email: u8, phone: u8) -> Vec<u8> {
        let (email, phone) = (format!("{email:02x}").repeat(32), format!("{phone:02x}").repeat(32));
        format!(
            "\x30junk<?xml version=\"1.0\"?><plist><dict><key>altDsID</key><string>{account}</string>\
             <key>ValidatedEmailHashes</key>\n\t<array>\n\t\t<string>{email}</string>\n\t</array>\
             <key>ValidatedPhoneHashes</key><array><string>{phone}</string></array></dict></plist>"
        )
        .into_bytes()
    }

    #[test]
    fn knows_a_contact_by_the_hashes_in_its_record() {
        let record = record("001-x", 0xab, 0xcd);
        let hashes = record_hashes(&record);
        assert_eq!(hashes, vec![[0xab; 32], [0xcd; 32]]);
        let proven = Some("001-x");
        let host = Account::new();
        assert!(!host.contact(Some(&record), proven), "no contacts yet");
        host.set_contacts(HashSet::from([[0xcd; 32]]));
        assert!(host.contact(Some(&record), proven));
        assert!(!host.contact(Some(&record), None), "an unproven record counts for nothing");
        assert!(!host.contact(None, proven));
        host.set_contacts(HashSet::from([[0x11; 32]]));
        assert!(!host.contact(Some(&record), proven));
    }

    /// Two Apple IDs on one PC, say two siblings': each sender gets exactly one receiver.
    #[test]
    fn one_receiver_answers_each_sender() {
        let (me, sibling) = (Account::new(), Account::new());
        me.install(AppleIdentity { record: record("001-me", 0x01, 0x02), ..Default::default() });
        sibling.install(AppleIdentity { record: record("001-sib", 0x03, 0x04), ..Default::default() });
        // Each has the other in their contacts, and a friend in both.
        let friend = record("001-friend", 0x05, 0x06);
        me.set_contacts(HashSet::from([[0x03; 32], [0x05; 32]]));
        sibling.set_contacts(HashSet::from([[0x01; 32], [0x06; 32]]));
        let both = [me.clone(), sibling.clone()];

        let mine = record("001-me", 0x01, 0x02);
        let theirs = record("001-sib", 0x03, 0x04);
        // An iPad of the sibling's lists the PC as the sibling's own, though it's my contact too.
        assert_eq!(answerer(&both, Some(&theirs), Some("001-sib")), Some((1, Relation::Own)));
        assert_eq!(answerer(&both, Some(&mine), Some("001-me")), Some((0, Relation::Own)));
        // A contact of both gets the first account only.
        assert_eq!(answerer(&both, Some(&friend), Some("001-friend")), Some((0, Relation::Contact)));
        // A replayed record proves nothing, and a stranger belongs to no one.
        assert_eq!(answerer(&both, Some(&theirs), None), None);
        assert_eq!(answerer(&both, Some(&record("001-z", 0x07, 0x08)), Some("001-z")), None);
        assert_eq!(answerer(&both, None, None), None);
        // Without an identity, an account knows no one.
        let empty = Account::new();
        empty.set_contacts(HashSet::from([[0x05; 32]]));
        assert_eq!(answerer(&[empty, me.clone()], Some(&friend), Some("001-friend")), Some((1, Relation::Contact)));
        assert!(both[0].same(&me) && !me.same(&sibling));
    }
}
