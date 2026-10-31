//! Apple GrandSlam 서버가 쓰는 SRP-6a 로그인: RFC 5054의 2048비트 그룹과 SHA-256에 Apple식 차이 두
//! 가지. 비밀 키 x에 계정 이름을 넣지 않아 x = H(s | H(":" | p))이고 p는 PBKDF2로 늘린 암호입니다
//! ([password_key] 참고). 증명은 RFC 2945의 것으로 M1 = H(H(N) xor H(g) | H(I) | s | A | B | K),
//! g는 N의 길이로 채웁니다.
use hmac::Hmac;
use num_bigint::BigUint;
use sha2::{Digest, Sha256};

/// RFC 5054의 2048비트 그룹: N, g = 2.
const N: &str = concat!(
    "AC6BDB41324A9A9BF166DE5E1389582FAF72B6651987EE07FC3192943DB56050A37329CBB4A099ED8193E075",
    "7767A13DD52312AB4B03310DCD7F48A9DA04FD50E8083969EDB767B0CF6095179A163AB3661A05FBD5FAAAE8",
    "2918A9962F0B93B855F97993EC975EEAA80D740ADBF4FF747359D041D5C33EA71D281E446B14773BCA97B43A",
    "23FB801676BD207A436C6481F1D2B9078717461A5B9D32E688F87748544523B524B0D57D5EA77A2775D2ECFA",
    "032CFBDBF52FB3786160279004E57AE6AF874E7303CE53299CCC041C7BC308D82A5698F3A8D0C38271AE35F8",
    "E9DBFBB694B5C803D89F7AE435DE236D525F54759B65E372FCD68EF20FA7111F9E4AFF73",
);
const G: u32 = 2;

fn n() -> BigUint {
    BigUint::parse_bytes(N.as_bytes(), 16).expect("RFC 5054's N")
}

/// 부분들을 차례로 이은 SHA-256.
fn h(parts: &[&[u8]]) -> Vec<u8> {
    let mut d = Sha256::new();
    for p in parts {
        d.update(p);
    }
    d.finalize().to_vec()
}

/// 암호를 대신하는 것: 암호 SHA-256 다이제스트에 서버가 준 솔트와 횟수로 PBKDF2-HMAC-SHA256.
/// "s2k"는 다이제스트 바이트를, "s2k_fo"는 그 소문자 16진수를 늘립니다.
pub fn password_key(password: &str, protocol: &str, salt: &[u8], rounds: u32) -> Vec<u8> {
    let digest = Sha256::digest(password.as_bytes());
    let input: Vec<u8> = if protocol == "s2k_fo" {
        digest.iter().map(|b| format!("{b:02x}")).collect::<String>().into_bytes()
    } else {
        digest.to_vec()
    };
    let mut key = vec![0u8; 32];
    pbkdf2::pbkdf2::<Hmac<Sha256>>(&input, salt, rounds, &mut key).expect("HMAC takes any key length");
    key
}

/// 로그인 한 번의 클라이언트 쪽: 비밀 a와 A = g^a.
pub struct Client {
    a: BigUint,
    a_pub: Vec<u8>,
}

/// 서버에 줄 클라이언트의 증명, 돌아와야 할 증명, 공유 키 K.
pub struct Proof {
    pub m1: Vec<u8>,
    m2: Vec<u8>,
    pub key: Vec<u8>,
}

impl Client {
    pub fn new() -> Client {
        Client::with_secret(&rand::random::<[u8; 32]>())
    }

    pub(crate) fn with_secret(a: &[u8]) -> Client {
        let a = BigUint::from_bytes_be(a);
        let a_pub = BigUint::from(G).modpow(&a, &n()).to_bytes_be();
        Client { a, a_pub }
    }

    /// 계정 이름과 함께 보내는 A.
    pub fn public(&self) -> &[u8] {
        &self.a_pub
    }

    /// 서버의 솔트와 B에 대한 증명: [password_key]의 key로 account에 로그인. 키를 예측 가능하게
    /// 만드는 B면 실패합니다.
    pub fn prove(&self, account: &str, key: &[u8], salt: &[u8], b_pub: &[u8]) -> Result<Proof, String> {
        let n = n();
        let b = BigUint::from_bytes_be(b_pub);
        if (&b % &n) == BigUint::ZERO {
            return Err("the server's B is not usable".into());
        }
        let n_bytes = n.to_bytes_be();
        let mut g_padded = vec![0u8; n_bytes.len()];
        *g_padded.last_mut().expect("N has bytes") = G as u8;
        let k = BigUint::from_bytes_be(&h(&[&n_bytes, &g_padded]));
        let u = BigUint::from_bytes_be(&h(&[&self.a_pub, &b.to_bytes_be()]));
        let x = BigUint::from_bytes_be(&h(&[salt, &h(&[b":", key])]));
        let g = BigUint::from(G);
        // S = (B - k g^x)^(a + u x), 모두 mod N.
        let kgx = (&k * g.modpow(&x, &n)) % &n;
        let base = ((&b + &n) - kgx) % &n;
        let s = base.modpow(&(&self.a + &u * &x), &n);
        let key = h(&[&s.to_bytes_be()]);
        let mut hn_xor_hg = h(&[&n_bytes]);
        for (a, b) in hn_xor_hg.iter_mut().zip(h(&[&g_padded])) {
            *a ^= b;
        }
        let m1 = h(&[&hn_xor_hg, &h(&[account.as_bytes()]), salt, &self.a_pub, &b.to_bytes_be(), &key]);
        let m2 = h(&[&self.a_pub, &m1, &key]);
        Ok(Proof { m1, m2, key })
    }
}

impl Proof {
    /// 서버의 M2는 서버가 암호의 검증자를 알고 있었음을 증명합니다.
    pub fn server_proved(&self, m2: &[u8]) -> bool {
        // 비밀 아님: 맞지 않으면 로그인이 끝날 뿐입니다.
        self.m2 == m2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 서버가 두는 방식으로 만든 검증자, 그리고 거기서 나온 양쪽의 키: 클라이언트의 키와 증명이
    /// 서버의 계산과 맞습니다.
    #[test]
    fn agrees_with_the_server_side() {
        let n = n();
        let g = BigUint::from(G);
        let salt = b"some salt bytes!";
        let key = password_key("correct horse", "s2k", salt, 1000);
        let x = BigUint::from_bytes_be(&h(&[salt, &h(&[b":", &key])]));
        let v = g.modpow(&x, &n);
        let client = Client::with_secret(&[7u8; 32]);
        // 서버: B = k v + g^b, S = (A v^u)^b.
        let n_bytes = n.to_bytes_be();
        let mut g_padded = vec![0u8; n_bytes.len()];
        *g_padded.last_mut().unwrap() = 2;
        let k = BigUint::from_bytes_be(&h(&[&n_bytes, &g_padded]));
        let b_secret = BigUint::from_bytes_be(&[9u8; 32]);
        let b_pub = ((&k * &v) + g.modpow(&b_secret, &n)) % &n;
        let u = BigUint::from_bytes_be(&h(&[client.public(), &b_pub.to_bytes_be()]));
        let s = (BigUint::from_bytes_be(client.public()) * v.modpow(&u, &n)).modpow(&b_secret, &n);
        let server_key = h(&[&s.to_bytes_be()]);
        let proof = client.prove("someone@example.com", &key, salt, &b_pub.to_bytes_be()).unwrap();
        assert_eq!(proof.key, server_key);
        let m2 = h(&[client.public(), &proof.m1, &server_key]);
        assert!(proof.server_proved(&m2));
        assert!(!proof.server_proved(&[0; 32]));
    }

    #[test]
    fn stretches_both_ways() {
        let salt = [1u8; 16];
        assert_ne!(password_key("pw", "s2k", &salt, 10), password_key("pw", "s2k_fo", &salt, 10));
        assert_eq!(password_key("pw", "s2k", &salt, 10).len(), 32);
    }

    #[test]
    fn refuses_a_zero_b() {
        let client = Client::new();
        assert!(client.prove("a", &[0; 32], b"s", &n().to_bytes_be()).is_err());
    }
}
