//! Whether an Apple ID sender is who its record says (§7): the record's CMS signature is Apple's
//! record-signing certificate's, and the TLS client certificate is the Apple ID certificate of the
//! same account. Without the second, a record a sender broadcast to every receiver it asked could
//! be replayed by anyone; without the first, anyone with an Apple ID certificate (Apple issues them
//! on request) could sign a record of their own.
use pki_types::{CertificateDer, UnixTime};
use webpki::{EndEntityCert, KeyPurposeIdIter, KeyUsage, anchor_from_trusted_cert};

/// Apple Root CA (SHA-256 B0B1730ECBC7FF4505142C49F1295E6EDA6BCAED7E2C68C5BE91B5A11001F024).
const APPLE_ROOT_CA: &[u8] = include_bytes!("apple_root_ca.der");
const APPLE_ID_PREFIX: &str = "com.apple.idms.appleid.prd.";
const APPLE_ID_ISSUER: &str = "Apple Application Integration Certification Authority";
const RECORD_SIGNER_PREFIX: &str = "AppleIDValidationRecordSigning";
const RECORD_SIGNER_ISSUER: &str = "Apple Application Integration 2 Certification Authority";

const OID_SIGNED_DATA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02];
const OID_MESSAGE_DIGEST: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x09, 0x04];
const OID_SHA1: &[u8] = &[0x2b, 0x0e, 0x03, 0x02, 0x1a];
const OID_SHA256: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01];
const OID_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
const OID_SHA1_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x05];
const OID_SHA256_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b];
const OID_CN: &[u8] = &[0x55, 0x04, 0x03];

/// The account a TLS client certificate chain proves: the altDsID of an Apple ID certificate that
/// Apple issued (it chains to Apple Root CA), or None.
pub fn tls_account(chain: &[CertificateDer<'_>]) -> Option<String> {
    let (leaf, intermediates) = chain.split_first()?;
    let cert = Cert::parse(leaf)?;
    let account = cert.subject_cn.strip_prefix(APPLE_ID_PREFIX)?.to_string();
    if cert.issuer_cn != APPLE_ID_ISSUER {
        return None;
    }
    chains_to_apple(leaf, intermediates, KeyUsage::client_auth()).then_some(account)
}

/// The altDsID of a validation record Apple signed, or None.
pub fn record_account(record: &[u8]) -> Option<String> {
    signed_by_apple(record).then(|| super::apple_id::alt_dsid(record)).flatten()
}

fn chains_to_apple(leaf: &CertificateDer<'_>, intermediates: &[CertificateDer<'_>], usage: impl webpki::ExtendedKeyUsageValidator) -> bool {
    let root = CertificateDer::from(APPLE_ROOT_CA);
    let Ok(anchor) = anchor_from_trusted_cert(&root) else { return false };
    let Ok(ee) = EndEntityCert::try_from(leaf) else { return false };
    ee.verify_for_usage(webpki::ALL_VERIFICATION_ALGS, &[anchor], intermediates, UnixTime::now(), usage, None, None).is_ok()
}

/// Any extended key usage: the record-signing certificate is pinned by name and issuer instead.
struct AnyUsage;

impl webpki::ExtendedKeyUsageValidator for AnyUsage {
    fn validate(&self, _: KeyPurposeIdIter<'_, '_>) -> Result<(), webpki::Error> {
        Ok(())
    }
}

/// The CMS SignedData of a validation record checks out: its message digest is the content's,
/// the signature over the signed attributes is the signer certificate's, and that certificate is
/// Apple's record-signing one.
fn signed_by_apple(record: &[u8]) -> bool {
    let Some((content_info, _)) = ber::read(record) else { return false };
    let mut parts = ber::children(content_info.content);
    if parts.next().map(|o| o.content) != Some(OID_SIGNED_DATA) {
        return false;
    }
    let Some(explicit) = parts.next() else { return false };
    let Some(signed_data) = ber::children(explicit.content).next() else { return false };
    let fields: Vec<ber::Tlv<'_>> = ber::children(signed_data.content).collect();
    let Some(encap) = fields.iter().find(|f| f.tag == 0x30) else { return false };
    let Some(content) = ber::children(encap.content).nth(1).and_then(|e| ber::children(e.content).next()).map(|o| ber::octets(&o)) else {
        return false;
    };
    let certs: Vec<CertificateDer<'_>> = fields
        .iter()
        .find(|f| f.tag == 0xa0)
        .map(|c| ber::children(c.content).map(|t| CertificateDer::from(t.raw)).collect())
        .unwrap_or_default();
    let Some(signer_infos) = fields.iter().rev().find(|f| f.tag == 0x31) else { return false };
    ber::children(signer_infos.content).any(|si| signer_ok(&si, &content, &certs))
}

fn signer_ok(signer_info: &ber::Tlv<'_>, content: &[u8], certs: &[CertificateDer<'_>]) -> bool {
    let fields: Vec<ber::Tlv<'_>> = ber::children(signer_info.content).collect();
    // version, sid, digestAlgorithm, [0] signedAttrs, signatureAlgorithm, signature
    let (Some(digest_alg), Some(attrs), Some(sig_alg), Some(signature)) = (
        fields.get(2).and_then(|a| ber::children(a.content).next()).map(|o| o.content),
        fields.iter().find(|f| f.tag == 0xa0),
        fields.iter().skip(3).find(|f| f.tag == 0x30).and_then(|a| ber::children(a.content).next()).map(|o| o.content),
        fields.iter().rev().find(|f| f.tag == 0x04).map(ber::octets),
    ) else {
        return false;
    };
    let digest = match digest_alg {
        OID_SHA1 => ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, content),
        OID_SHA256 => ring::digest::digest(&ring::digest::SHA256, content),
        _ => return false,
    };
    let declared = ber::children(attrs.content).find_map(|attr| {
        let mut a = ber::children(attr.content);
        (a.next()?.content == OID_MESSAGE_DIGEST).then(|| a.next().and_then(|set| ber::children(set.content).next()).map(|o| ber::octets(&o)))?
    });
    if declared.as_deref() != Some(digest.as_ref()) {
        return false;
    }
    let algorithm: &dyn ring::signature::VerificationAlgorithm = match (sig_alg, digest_alg) {
        (OID_RSA, OID_SHA1) | (OID_SHA1_RSA, _) => &ring::signature::RSA_PKCS1_2048_8192_SHA1_FOR_LEGACY_USE_ONLY,
        (OID_RSA, OID_SHA256) | (OID_SHA256_RSA, _) => &ring::signature::RSA_PKCS1_2048_8192_SHA256,
        _ => return false,
    };
    // The signature covers the attributes DER-encoded as a SET, not as the [0] they're sent in.
    let Some(mut signed) = attrs.der_definite() else { return false };
    signed[0] = 0x31;
    certs.iter().enumerate().any(|(i, der)| {
        let Some(cert) = Cert::parse(der) else { return false };
        if !cert.subject_cn.starts_with(RECORD_SIGNER_PREFIX) || cert.issuer_cn != RECORD_SIGNER_ISSUER {
            return false;
        }
        let key = ring::signature::UnparsedPublicKey::new(algorithm, &cert.rsa_key);
        if key.verify(&signed, &signature).is_err() {
            return false;
        }
        let others: Vec<CertificateDer<'_>> = certs.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, c)| c.clone()).collect();
        chains_to_apple(der, &others, AnyUsage)
    })
}

/// What we need of an X.509 certificate.
struct Cert {
    subject_cn: String,
    issuer_cn: String,
    /// The subjectPublicKey bits: an RSAPublicKey for RSA keys.
    rsa_key: Vec<u8>,
}

impl Cert {
    fn parse(der: &[u8]) -> Option<Cert> {
        let (cert, _) = ber::read(der)?;
        let tbs = ber::children(cert.content).next()?;
        let mut f: Vec<ber::Tlv<'_>> = ber::children(tbs.content).collect();
        if f.first().is_some_and(|v| v.tag == 0xa0) {
            f.remove(0);
        }
        // serialNumber, signature, issuer, validity, subject, subjectPublicKeyInfo
        let (issuer, subject, spki) = (f.get(2)?, f.get(4)?, f.get(5)?);
        let mut key = ber::children(spki.content);
        let _algorithm = key.next()?;
        let bits = key.next()?;
        Some(Cert { subject_cn: cn(subject)?, issuer_cn: cn(issuer).unwrap_or_default(), rsa_key: bits.content.get(1..)?.to_vec() })
    }
}

fn cn(name: &ber::Tlv<'_>) -> Option<String> {
    ber::children(name.content).flat_map(|rdn| ber::children(rdn.content)).find_map(|atv| {
        let mut a = ber::children(atv.content);
        (a.next()?.content == OID_CN).then(|| a.next().map(|v| String::from_utf8_lossy(v.content).into_owned()))?
    })
}

/// Just enough BER for CMS (Apple's records use indefinite lengths) and DER certificates.
mod ber {
    #[derive(Clone, Copy, Debug)]
    pub struct Tlv<'a> {
        pub tag: u8,
        /// The contents, without the end-of-contents octets of an indefinite length.
        pub content: &'a [u8],
        /// The whole element, header included.
        pub raw: &'a [u8],
        indefinite: bool,
    }

    impl Tlv<'_> {
        /// The element with a definite-length header, as DER needs it.
        pub fn der_definite(&self) -> Option<Vec<u8>> {
            if !self.indefinite {
                return Some(self.raw.to_vec());
            }
            let mut out = vec![self.tag];
            let n = self.content.len();
            match n {
                0..=0x7f => out.push(n as u8),
                0x80..=0xff => out.extend([0x81, n as u8]),
                0x100..=0xffff => out.extend([0x82, (n >> 8) as u8, n as u8]),
                _ => out.extend([0x83, (n >> 16) as u8, (n >> 8) as u8, n as u8]),
            }
            out.extend_from_slice(self.content);
            Some(out)
        }
    }

    /// The element at the start of input, and what follows it.
    pub fn read(input: &[u8]) -> Option<(Tlv<'_>, &[u8])> {
        let tag = *input.first()?;
        if tag & 0x1f == 0x1f {
            return None;
        }
        let first = *input.get(1)?;
        let (len, header) = match first {
            0x80 => (None, 2),
            0..=0x7f => (Some(first as usize), 2),
            0x81..=0x84 => {
                let n = (first & 0x7f) as usize;
                let bytes = input.get(2..2 + n)?;
                (Some(bytes.iter().fold(0usize, |a, b| (a << 8) | *b as usize)), 2 + n)
            }
            _ => return None,
        };
        match len {
            Some(len) => {
                let end = header.checked_add(len)?;
                let content = input.get(header..end)?;
                Some((Tlv { tag, content, raw: &input[..end], indefinite: false }, &input[end..]))
            }
            None => {
                if tag & 0x20 == 0 {
                    return None;
                }
                let mut rest = &input[header..];
                loop {
                    if rest.starts_with(&[0, 0]) {
                        let content_len = input.len() - header - rest.len();
                        let end = header + content_len + 2;
                        let content = &input[header..header + content_len];
                        return Some((Tlv { tag, content, raw: &input[..end], indefinite: true }, &input[end..]));
                    }
                    let (_, after) = read(rest)?;
                    rest = after;
                }
            }
        }
    }

    pub fn children(mut content: &[u8]) -> impl Iterator<Item = Tlv<'_>> {
        std::iter::from_fn(move || {
            let (t, rest) = read(content)?;
            content = rest;
            Some(t)
        })
    }

    /// An OCTET STRING's bytes, joining the pieces of a constructed one.
    pub fn octets(t: &Tlv<'_>) -> Vec<u8> {
        if t.tag & 0x20 == 0 {
            return t.content.to_vec();
        }
        children(t.content).flat_map(|c| octets(&c)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real identity, kept out of the repository (its record carries contact hashes):
    /// OWLY_TEST_IDENTITY is a folder with validation_record.cms, certificate.der and
    /// intermediate.der, as winid writes them.
    #[test]
    #[ignore = "needs OWLY_TEST_IDENTITY"]
    fn proves_a_real_apple_id_and_nothing_else() {
        let dir = std::path::PathBuf::from(std::env::var("OWLY_TEST_IDENTITY").expect("OWLY_TEST_IDENTITY"));
        let read = |name: &str| std::fs::read(dir.join(name)).unwrap();
        let (record, cert, intermediate) = (read("validation_record.cms"), read("certificate.der"), read("intermediate.der"));
        let account = record_account(&record).expect("Apple signed the record");
        assert_eq!(Some(account.clone()), super::super::apple_id::alt_dsid(&record));
        let chain = [CertificateDer::from(cert.clone()), CertificateDer::from(intermediate)];
        assert_eq!(tls_account(&chain), Some(account), "the TLS certificate is the same account's");
        assert_eq!(tls_account(&chain[..1]), None, "no chain to Apple Root CA without the intermediate");
        // One byte of the content changed: the message digest no longer matches.
        let at = record.windows(8).position(|w| w == b"altDsID<").unwrap();
        let mut forged = record.clone();
        forged[at + 2] ^= 0x20;
        assert_eq!(record_account(&forged), None);
        // The record signed by an Apple ID certificate instead would fail the signer pin; a
        // truncated one fails to parse.
        assert_eq!(record_account(&record[..record.len() / 2]), None);
    }

    #[test]
    fn reads_indefinite_lengths() {
        // SEQUENCE (indefinite) { OCTET STRING (constructed, indefinite) { "ab", "c" } }
        let ber = [0x30, 0x80, 0x24, 0x80, 0x04, 0x02, b'a', b'b', 0x04, 0x01, b'c', 0, 0, 0, 0];
        let (seq, rest) = ber::read(&ber).unwrap();
        assert!(rest.is_empty());
        let s = ber::children(seq.content).next().unwrap();
        assert_eq!(ber::octets(&s), b"abc");
        // The outer header becomes definite over its 11 content bytes.
        assert_eq!(seq.der_definite().unwrap()[..2], [0x30, 11]);
    }
}
