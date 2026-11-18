//! Key packages from an approved joiner that are validly signed but harmful to the host.
//!
//! The editor takes a key package apart (RFC 9420 section 10), changes it and signs it again with
//! the joiner's device key, so these packages pass every signature check in `Group::add`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use veil_group::group::MAX_KEY_PACKAGE_LEN;
use veil_group::{Group, GroupError, Joiner};
use veil_security::Identity;

fn length_prefix(n: usize) -> Vec<u8> {
    if n < 1 << 6 {
        vec![u8::try_from(n).unwrap()]
    } else if n < 1 << 14 {
        (0x4000 | u16::try_from(n).unwrap()).to_be_bytes().to_vec()
    } else {
        (0x8000_0000 | u32::try_from(n).unwrap()).to_be_bytes().to_vec()
    }
}

fn put_opaque(out: &mut Vec<u8>, data: &[u8]) {
    out.extend_from_slice(&length_prefix(data.len()));
    out.extend_from_slice(data);
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn fixed(&mut self, n: usize) -> &'a [u8] {
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        head
    }

    fn opaque(&mut self) -> Vec<u8> {
        let n = match self.0[0] >> 6 {
            0 => usize::from(self.fixed(1)[0]),
            1 => {
                let b = self.fixed(2);
                usize::from(u16::from_be_bytes([b[0] & 0x3f, b[1]]))
            }
            2 => {
                let b = self.fixed(4);
                usize::try_from(u32::from_be_bytes([b[0] & 0x3f, b[1], b[2], b[3]])).unwrap()
            }
            _ => panic!("not an MLS length prefix"),
        };
        self.fixed(n).to_vec()
    }
}

struct Forged {
    head: Vec<u8>,
    init_key: Vec<u8>,
    encryption_key: Vec<u8>,
    signature_key: Vec<u8>,
    credential_type: Vec<u8>,
    credential_identity: Vec<u8>,
    /// Versions, cipher suites, extensions, proposals, credentials.
    capabilities: [Vec<u8>; 5],
    lifetime: Vec<u8>,
    leaf_extensions: Vec<u8>,
    package_extensions: Vec<u8>,
}

impl Forged {
    fn honest(identity: &Identity) -> Self {
        let bytes = Joiner::new(identity).unwrap().key_package().unwrap();
        let mut c = Cursor(&bytes);
        let head = c.fixed(4).to_vec();
        let init_key = c.opaque();
        let encryption_key = c.opaque();
        let signature_key = c.opaque();
        let credential_type = c.fixed(2).to_vec();
        let credential_identity = c.opaque();
        let capabilities = [c.opaque(), c.opaque(), c.opaque(), c.opaque(), c.opaque()];
        assert_eq!(c.fixed(1), [1], "leaf node source is key_package");
        let lifetime = c.fixed(16).to_vec();
        let leaf_extensions = c.opaque();
        let _leaf_signature = c.opaque();
        let package_extensions = c.opaque();
        let _package_signature = c.opaque();
        assert!(c.0.is_empty());
        Self {
            head,
            init_key,
            encryption_key,
            signature_key,
            credential_type,
            credential_identity,
            capabilities,
            lifetime,
            leaf_extensions,
            package_extensions,
        }
    }

    fn leaf_to_be_signed(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_opaque(&mut out, &self.encryption_key);
        put_opaque(&mut out, &self.signature_key);
        out.extend_from_slice(&self.credential_type);
        put_opaque(&mut out, &self.credential_identity);
        for list in &self.capabilities {
            put_opaque(&mut out, list);
        }
        out.push(1);
        out.extend_from_slice(&self.lifetime);
        put_opaque(&mut out, &self.leaf_extensions);
        out
    }

    fn signed(&self, identity: &Identity) -> Vec<u8> {
        let sign = |label: &str, content: &[u8]| {
            let mut message = Vec::new();
            put_opaque(&mut message, format!("MLS 1.0 {label}").as_bytes());
            put_opaque(&mut message, content);
            identity.sign(&message).to_vec()
        };
        let leaf = self.leaf_to_be_signed();
        let mut out = self.head.clone();
        put_opaque(&mut out, &self.init_key);
        out.extend_from_slice(&leaf);
        put_opaque(&mut out, &sign("LeafNodeTBS", &leaf));
        put_opaque(&mut out, &self.package_extensions);
        let signature = sign("KeyPackageTBS", &out);
        put_opaque(&mut out, &signature);
        out
    }

    /// Grows the package to `total` bytes, or one less, with GREASE cipher-suite values.
    fn padded(mut self, identity: &Identity, total: usize) -> Vec<u8> {
        loop {
            let package = self.signed(identity);
            if package.len() + 2 > total {
                return package;
            }
            let room = (total - package.len()) / 2;
            let entries = if room > 4 { room - 2 } else { 1 };
            for _ in 0..entries {
                self.capabilities[1].extend_from_slice(&[0x0a, 0x0a]);
            }
        }
    }
}

fn host() -> (Identity, Group) {
    let identity = Identity::generate();
    let group = Group::create(&identity, [9; 16]).unwrap();
    (identity, group)
}

fn honest_package(identity: &Identity) -> Vec<u8> {
    Joiner::new(identity).unwrap().key_package().unwrap()
}

#[test]
fn the_editor_makes_packages_the_host_accepts() {
    let (_, mut group) = host();
    let joiner = Identity::generate();
    let package = Forged::honest(&joiner).signed(&joiner);
    group.add(&package, &joiner.public()).unwrap();
}

#[test]
fn an_oversized_key_package_is_refused_and_the_epoch_stays() {
    let (_, mut group) = host();
    let member = Identity::generate();
    group.add(&honest_package(&member), &member.public()).unwrap();
    let epoch = group.epoch();

    let attacker = Identity::generate();
    let package = Forged::honest(&attacker).padded(&attacker, 65_000);
    assert!(package.len() > MAX_KEY_PACKAGE_LEN);
    assert_eq!(
        group.add(&package, &attacker.public()).err(),
        Some(GroupError::Malformed("key package too long"))
    );
    assert_eq!(group.epoch(), epoch);

    let next = Identity::generate();
    group.add(&honest_package(&next), &next.public()).unwrap();
    group.remove(&member.public()).unwrap();
    assert_eq!(group.epoch(), epoch + 2);
}

#[test]
fn a_full_call_of_packages_at_the_limit_can_still_be_delivered() {
    let (_, mut group) = host();
    for _ in 1..veil_group::MAX_MEMBERS {
        let joiner = Identity::generate();
        let package = Forged::honest(&joiner).padded(&joiner, MAX_KEY_PACKAGE_LEN);
        assert!(package.len() <= MAX_KEY_PACKAGE_LEN);
        group.add(&package, &joiner.public()).unwrap();
    }
    assert_eq!(group.members().len(), veil_group::MAX_MEMBERS);
}
