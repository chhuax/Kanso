//! 算法表与旧算法判定（ssh.md §2.1、§2.2）。
//!
//! 顺序由**客户端**决定最终选择，所以"现代在前、旧算法在后"就能保证服务器只要支持更好的算法就不会
//! 落到旧算法上。所有名字都来自 russh 的常量，没有手写字符串：russh 不支持的算法在编译期就会暴露。
//!
//! 刻意不支持：`3des-cbc`、`hmac-md5`、`ssh-dss`（ssh.md §2.1）。它们既不在 russh 的构建里
//! （`des` / `dsa` feature 未开），也不出现在本表里。

use std::borrow::Cow;

use kanso_types::message::AlgorithmCategory;
use russh::Preferred;
use russh::cipher;
use russh::compression;
use russh::kex;
use russh::keys::{Algorithm, EcdsaCurve, HashAlg};
use russh::mac;

/// DH group-exchange 能接受的最小群（位）。与 OpenSSH 下限一致；russh 的客户端配置也拒绝更小的值。
pub const GEX_MIN_BITS: usize = 2048;
/// DH group-exchange 的首选群大小（位）。
pub const GEX_PREFERRED_BITS: usize = 3072;
/// DH group-exchange 能接受的最大群（位）。
pub const GEX_MAX_BITS: usize = 8192;

/// 现代密钥交换算法，顺序参照 OpenSSH 默认列表。
///
/// 包含 NIST ECDH（nistp256/384/521）：许多网络设备只支持这几条曲线，而 russh 的默认列表里没有。
const MODERN_KEX: &[kex::Name] = &[
    kex::MLKEM768X25519_SHA256,
    kex::CURVE25519,
    kex::CURVE25519_PRE_RFC_8731,
    kex::ECDH_SHA2_NISTP256,
    kex::ECDH_SHA2_NISTP384,
    kex::ECDH_SHA2_NISTP521,
    kex::DH_GEX_SHA256,
    kex::DH_G16_SHA512,
    kex::DH_G18_SHA512,
    kex::DH_G17_SHA512,
    kex::DH_G15_SHA512,
    kex::DH_G14_SHA256,
];

/// 旧密钥交换算法。
///
/// `group14-sha1` 必须排在 `group-exchange-sha1` 前面：用 group-exchange 时要协商完才知道群有多大，
/// 设备的群若小于下限，连接会直接失败，没有机会退回到它同样支持的固定 2048 位群。
const LEGACY_KEX: &[kex::Name] = &[kex::DH_G14_SHA1, kex::DH_GEX_SHA1, kex::DH_G1_SHA1];

/// RFC 8308 扩展协商标记与 OpenSSH strict-kex 标记（防御 Terrapin）。
///
/// russh 发出 KEXINIT 时会按角色过滤，只会发出 `-c` 版本；两个角色的名字都放进来，
/// 是为了与 russh 默认表保持同一形态（升级后行为一致）。
const KEX_EXTENSIONS: &[kex::Name] = &[
    kex::EXTENSION_SUPPORT_AS_CLIENT,
    kex::EXTENSION_SUPPORT_AS_SERVER,
    kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT,
    kex::EXTENSION_OPENSSH_STRICT_KEX_AS_SERVER,
];

const MODERN_CIPHERS: &[cipher::Name] = &[
    cipher::CHACHA20_POLY1305,
    cipher::AES_256_GCM,
    cipher::AES_128_GCM,
    cipher::AES_256_CTR,
    cipher::AES_192_CTR,
    cipher::AES_128_CTR,
];

const LEGACY_CIPHERS: &[cipher::Name] = &[
    cipher::AES_256_CBC,
    cipher::AES_192_CBC,
    cipher::AES_128_CBC,
];

const MODERN_MACS: &[mac::Name] = &[
    mac::HMAC_SHA512_ETM,
    mac::HMAC_SHA256_ETM,
    mac::HMAC_SHA512,
    mac::HMAC_SHA256,
];

const LEGACY_MACS: &[mac::Name] = &[mac::HMAC_SHA1_ETM, mac::HMAC_SHA1];

/// 现代主机密钥算法。
fn modern_host_keys() -> Vec<Algorithm> {
    vec![
        Algorithm::Ed25519,
        Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP256,
        },
        Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP384,
        },
        Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP521,
        },
        Algorithm::Rsa {
            hash: Some(HashAlg::Sha512),
        },
        Algorithm::Rsa {
            hash: Some(HashAlg::Sha256),
        },
    ]
}

/// 旧主机密钥算法：`ssh-rsa`（SHA-1 签名）。
fn legacy_host_keys() -> Vec<Algorithm> {
    vec![Algorithm::Rsa { hash: None }]
}

/// 判断某个算法名是否属于"旧算法"（ssh.md §2.2），按类别区分。
///
/// 这是 §2.2 "Legacy SSH" 提示的唯一判据，也是本模块算法表的一部分：
/// 旧算法 = 排在所有现代算法之后、仅为兼容老设备而保留的那批。
/// AEAD 加密时 MAC 并未使用，是否忽略它由 [`legacy_algorithms_used`] 处理，不在这里。
/// 压缩类别没有旧算法。
pub fn is_legacy(kind: AlgorithmCategory, name: &str) -> bool {
    match kind {
        AlgorithmCategory::Kex => LEGACY_KEX.iter().any(|n| n.as_ref() == name),
        AlgorithmCategory::HostKey => legacy_host_keys().iter().any(|a| a.as_str() == name),
        AlgorithmCategory::Cipher => LEGACY_CIPHERS.iter().any(|n| n.as_ref() == name),
        AlgorithmCategory::Mac => LEGACY_MACS.iter().any(|n| n.as_ref() == name),
        AlgorithmCategory::Compression => false,
    }
}

/// 该加密算法是否为 AEAD（chacha20-poly1305、aes-gcm）：此时 MAC 字段不参与，不能计入旧算法。
pub fn is_aead_cipher(name: &str) -> bool {
    matches!(
        name,
        "chacha20-poly1305@openssh.com" | "aes128-gcm@openssh.com" | "aes256-gcm@openssh.com"
    )
}

/// 一次连接实际协商出的算法（名字为 SSH wire 名）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NegotiatedAlgorithms {
    /// 密钥交换。
    pub kex: String,
    /// 主机密钥 / 签名算法（RSA 为 `ssh-rsa`、`rsa-sha2-256`、`rsa-sha2-512` 之一）。
    pub host_key: String,
    /// 加密算法。russh 对两个方向使用同一个加密算法（取客户端到服务器方向协商的结果）。
    pub cipher: String,
    /// 客户端到服务器方向的 MAC；AEAD 加密时为 `none`。
    pub client_mac: String,
    /// 服务器到客户端方向的 MAC；AEAD 加密时为 `none`。
    pub server_mac: String,
}

/// 从协商结果里挑出用到的旧算法（去重，按 kex、主机密钥、加密、MAC 的顺序）。
///
/// AEAD 加密时 MAC 并未实际使用，不能因为协商结果里出现了 hmac-sha1 就报旧算法（ssh.md §2.2）。
pub fn legacy_algorithms_used(n: &NegotiatedAlgorithms) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |kind: AlgorithmCategory, name: &str| {
        if is_legacy(kind, name) && !out.iter().any(|x| x == name) {
            out.push(name.to_owned());
        }
    };
    push(AlgorithmCategory::Kex, &n.kex);
    push(AlgorithmCategory::HostKey, &n.host_key);
    push(AlgorithmCategory::Cipher, &n.cipher);
    if !is_aead_cipher(&n.cipher) {
        push(AlgorithmCategory::Mac, &n.client_mac);
        push(AlgorithmCategory::Mac, &n.server_mac);
    }
    out
}

/// 是否为扩展标记一类的伪算法（`ext-info-*`、`kex-strict-*`）。
///
/// 协商失败时列服务器提供的算法要去掉它们（ssh.md §2.3）。
pub fn is_pseudo_algorithm(name: &str) -> bool {
    name.starts_with("ext-info-") || name.starts_with("kex-strict-")
}

/// 构造 Kanso 的 russh 算法偏好：现代算法在前、旧算法在后。
///
/// 用 `..Preferred::default()` 补齐其余字段：russh 新增字段（如 `host_key_certificates`）
/// 时沿用它的默认值，而不是让这里编译失败或悄悄清空。证书主机密钥保持关闭——
/// 开启后服务器会用证书自证身份，而我们没有 CA 信任模型可用。
pub fn preferred() -> Preferred {
    let mut kex_list: Vec<kex::Name> = MODERN_KEX.to_vec();
    kex_list.extend_from_slice(LEGACY_KEX);
    kex_list.extend_from_slice(KEX_EXTENSIONS);

    let mut host_keys = modern_host_keys();
    host_keys.extend(legacy_host_keys());

    let mut ciphers: Vec<cipher::Name> = MODERN_CIPHERS.to_vec();
    ciphers.extend_from_slice(LEGACY_CIPHERS);

    let mut macs: Vec<mac::Name> = MODERN_MACS.to_vec();
    macs.extend_from_slice(LEGACY_MACS);

    Preferred {
        kex: Cow::Owned(kex_list),
        key: Cow::Owned(host_keys),
        cipher: Cow::Owned(ciphers),
        mac: Cow::Owned(macs),
        // 压缩沿用 russh 默认（none 优先）：规格没有要求改动。
        compression: Cow::Owned(russh_default_compression()),
        ..Preferred::default()
    }
}

fn russh_default_compression() -> Vec<compression::Name> {
    Preferred::default().compression.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kex_names(p: &Preferred) -> Vec<String> {
        p.kex.iter().map(|n| n.as_ref().to_owned()).collect()
    }
    fn key_names(p: &Preferred) -> Vec<String> {
        p.key.iter().map(|a| a.as_str().to_owned()).collect()
    }
    fn cipher_names(p: &Preferred) -> Vec<String> {
        p.cipher.iter().map(|n| n.as_ref().to_owned()).collect()
    }
    fn mac_names(p: &Preferred) -> Vec<String> {
        p.mac.iter().map(|n| n.as_ref().to_owned()).collect()
    }

    /// 升级 russh 后，它默认支持的算法不能被我们的表悄悄丢掉（ssh.md §12）。
    #[test]
    fn contains_every_russh_default_algorithm() {
        let ours = preferred();
        let theirs = Preferred::default();
        for n in theirs.kex.iter() {
            assert!(ours.kex.contains(n), "缺少 kex {}", n.as_ref());
        }
        for a in theirs.key.iter() {
            assert!(ours.key.contains(a), "缺少 host key {a}");
        }
        for n in theirs.cipher.iter() {
            assert!(ours.cipher.contains(n), "缺少 cipher {}", n.as_ref());
        }
        for n in theirs.mac.iter() {
            assert!(ours.mac.contains(n), "缺少 mac {}", n.as_ref());
        }
        for n in theirs.compression.iter() {
            assert!(
                ours.compression.contains(n),
                "缺少 compression {}",
                n.as_ref()
            );
        }
    }

    /// 旧算法都排在同类的所有现代算法之后（ssh.md §2.1）。
    #[test]
    fn legacy_algorithms_come_after_all_modern_ones() {
        let p = preferred();
        let check = |kind: AlgorithmCategory, names: Vec<String>| {
            let first_legacy = names.iter().position(|n| is_legacy(kind, n));
            let last_modern = names
                .iter()
                .rposition(|n| !is_legacy(kind, n) && !is_pseudo_algorithm(n));
            if let (Some(l), Some(m)) = (first_legacy, last_modern) {
                assert!(
                    m < l,
                    "{kind:?}: 旧算法 {} 排在现代算法 {} 之前",
                    names[l],
                    names[m]
                );
            }
            assert!(
                names.iter().any(|n| is_legacy(kind, n)),
                "{kind:?} 应至少有一个旧算法"
            );
        };
        check(AlgorithmCategory::Kex, kex_names(&p));
        check(AlgorithmCategory::HostKey, key_names(&p));
        check(AlgorithmCategory::Cipher, cipher_names(&p));
        check(AlgorithmCategory::Mac, mac_names(&p));
    }

    /// group14-sha1 在 group-exchange-sha1 之前，再到 group1-sha1（ssh.md §2.1）。
    #[test]
    fn legacy_kex_order() {
        let names = kex_names(&preferred());
        let pos = |s: &str| names.iter().position(|n| n == s).unwrap();
        assert!(pos("diffie-hellman-group14-sha1") < pos("diffie-hellman-group-exchange-sha1"));
        assert!(pos("diffie-hellman-group-exchange-sha1") < pos("diffie-hellman-group1-sha1"));
    }

    #[test]
    fn modern_kex_includes_nist_curves_and_pq_hybrid() {
        let names = kex_names(&preferred());
        for n in [
            "mlkem768x25519-sha256",
            "curve25519-sha256",
            "ecdh-sha2-nistp256",
            "ecdh-sha2-nistp384",
            "ecdh-sha2-nistp521",
            "diffie-hellman-group-exchange-sha256",
            "diffie-hellman-group16-sha512",
            "diffie-hellman-group18-sha512",
            "diffie-hellman-group14-sha256",
        ] {
            assert!(names.iter().any(|x| x == n), "缺少 {n}");
        }
        // 后量子混合排第一，curve25519 紧随其后。
        assert_eq!(names[0], "mlkem768x25519-sha256");
        assert_eq!(names[1], "curve25519-sha256");
    }

    /// 不支持的算法不能出现在任何列表里（ssh.md §2.1）。
    #[test]
    fn unsupported_algorithms_are_absent() {
        let p = preferred();
        assert!(!cipher_names(&p).iter().any(|n| n == "3des-cbc"));
        assert!(!mac_names(&p).iter().any(|n| n.contains("md5")));
        assert!(!key_names(&p).iter().any(|n| n == "ssh-dss"));
    }

    #[test]
    fn spec_legacy_names_present() {
        let p = preferred();
        for n in ["aes128-cbc", "aes192-cbc", "aes256-cbc"] {
            assert!(cipher_names(&p).iter().any(|x| x == n));
        }
        for n in ["hmac-sha1", "hmac-sha1-etm@openssh.com"] {
            assert!(mac_names(&p).iter().any(|x| x == n));
        }
        assert!(key_names(&p).iter().any(|x| x == "ssh-rsa"));
    }

    #[test]
    fn extension_markers_present_for_client_role() {
        let names = kex_names(&preferred());
        assert!(names.iter().any(|n| n == "ext-info-c"));
        assert!(names.iter().any(|n| n == "kex-strict-c-v00@openssh.com"));
    }

    #[test]
    fn is_legacy_table() {
        assert!(is_legacy(
            AlgorithmCategory::Kex,
            "diffie-hellman-group1-sha1"
        ));
        assert!(!is_legacy(AlgorithmCategory::Kex, "ecdh-sha2-nistp256"));
        assert!(is_legacy(AlgorithmCategory::HostKey, "ssh-rsa"));
        assert!(!is_legacy(AlgorithmCategory::HostKey, "rsa-sha2-256"));
        assert!(is_legacy(AlgorithmCategory::Cipher, "aes128-cbc"));
        assert!(!is_legacy(AlgorithmCategory::Cipher, "aes128-ctr"));
        assert!(is_legacy(AlgorithmCategory::Mac, "hmac-sha1"));
        assert!(!is_legacy(AlgorithmCategory::Mac, "hmac-sha2-256"));
        assert!(!is_legacy(AlgorithmCategory::Compression, "zlib"));
    }

    #[test]
    fn aead_cipher_ignores_mac_for_legacy_detection() {
        let n = NegotiatedAlgorithms {
            kex: "curve25519-sha256".into(),
            host_key: "ssh-ed25519".into(),
            cipher: "aes256-gcm@openssh.com".into(),
            client_mac: "hmac-sha1".into(),
            server_mac: "hmac-sha1".into(),
        };
        assert!(legacy_algorithms_used(&n).is_empty());

        let n = NegotiatedAlgorithms {
            cipher: "aes256-ctr".into(),
            ..n
        };
        assert_eq!(legacy_algorithms_used(&n), vec!["hmac-sha1".to_owned()]);
    }

    #[test]
    fn pseudo_algorithms() {
        assert!(is_pseudo_algorithm("ext-info-s"));
        assert!(is_pseudo_algorithm("kex-strict-s-v00@openssh.com"));
        assert!(!is_pseudo_algorithm("curve25519-sha256"));
    }
}
