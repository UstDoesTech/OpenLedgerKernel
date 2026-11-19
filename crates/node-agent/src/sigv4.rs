//! AWS Signature Version 4, for the one service the agent calls. Checked against the example in
//! AWS's documentation (see the tests).

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct Credentials {
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,
}

// Never printed: a debug line must not leak a key.
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials(..)")
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = <HmacSha256 as KeyInit>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

pub struct Signed {
    pub authorization: String,
    pub signed_headers: String,
    pub canonical_request_hash: String,
    pub signature: String,
}

/// Signs a request. `headers` must hold every header that will be sent and signed, including
/// `host` and `x-amz-date`. `amz_date` looks like `20150830T123600Z`.
#[allow(clippy::too_many_arguments)]
pub fn sign(
    credentials: &Credentials,
    region: &str,
    service: &str,
    method: &str,
    path: &str,
    query: &str,
    headers: &[(String, String)],
    body: &[u8],
    amz_date: &str,
) -> Signed {
    let date = &amz_date[..8];
    let mut canonical: Vec<(String, String)> = headers
        .iter()
        .map(|(n, v)| {
            (
                n.to_ascii_lowercase(),
                v.split_whitespace().collect::<Vec<_>>().join(" "),
            )
        })
        .collect();
    canonical.sort();
    let canonical_headers: String = canonical
        .iter()
        .map(|(n, v)| format!("{n}:{v}\n"))
        .collect();
    let signed_headers = canonical
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_request = format!(
        "{method}\n{path}\n{query}\n{canonical_headers}\n{signed_headers}\n{}",
        sha256_hex(body)
    );
    let canonical_request_hash = sha256_hex(canonical_request.as_bytes());
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{canonical_request_hash}");
    let key = hmac(
        &hmac(
            &hmac(
                &hmac(format!("AWS4{}", credentials.secret_key).as_bytes(), date),
                region,
            ),
            service,
        ),
        "aws4_request",
    );
    let signature = hex(&hmac(&key, &to_sign));
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        credentials.access_key
    );
    Signed {
        authorization,
        signed_headers,
        canonical_request_hash,
        signature,
    }
}

/// `20150830T123600Z` for a count of seconds since 1970, in UTC.
pub fn amz_date(unix_seconds: u64) -> String {
    let days = (unix_seconds / 86_400) as i64;
    let secs = unix_seconds % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}
