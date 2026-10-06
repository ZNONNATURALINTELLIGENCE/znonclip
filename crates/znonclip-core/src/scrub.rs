//! Pre-prompt secret scrubbing: replace known credential shapes with
//! `[REDACTED:<kind>]` before text goes into a model's context window.
//!
//! This is a fixed list of key prefixes and labelled patterns, not a complete
//! secret scanner. It deliberately has **no** bare hex or bare base64 rule, so
//! git hashes and checksums in logs survive. Patterns were reviewed in the
//! 2026-10-06 pre-launch audit; each has a must-match and a must-not-match test.

use std::sync::LazyLock;

use regex::{Captures, Regex};

/// One credential shape and its redaction label.
struct Rule {
    re: Regex,
    label: &'static str,
    /// Redact only if the matched secret contains a digit (cuts false
    /// positives on long words such as `sk-project-architecture-...`).
    needs_digit: bool,
    /// Capture group holding the secret; text outside it (a `key =` label)
    /// is kept. 0 = the whole match.
    group: usize,
}

fn rule(pattern: &str, label: &'static str, needs_digit: bool, group: usize) -> Rule {
    Rule {
        re: Regex::new(pattern).expect("scrub pattern compiles"),
        label,
        needs_digit,
        group,
    }
}

/// Most specific first: a later, broader rule never sees text an earlier one
/// already replaced, because `[REDACTED:...]` is outside every key alphabet.
static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        rule(
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
            "private-key",
            false,
            0,
        ),
        rule(r"sk-ant-(?:api|admin)\d{2}-[A-Za-z0-9_-]{20,}", "anthropic-key", false, 0),
        rule(r"sk-(?:proj|svcacct|admin)-[A-Za-z0-9_-]{40,}", "openai-key", false, 0),
        rule(r"sk-or-v1-[A-Za-z0-9]{32,}", "openrouter-key", false, 0),
        // Plain `sk-` keys (legacy OpenAI and several OpenAI-compatible providers).
        rule(r"\bsk-[A-Za-z0-9]{32,}\b", "api-key", true, 0),
        // xAI: prefix from model knowledge, not yet checked against xAI docs.
        rule(r"\bxai-[A-Za-z0-9]{32,}\b", "xai-key", true, 0),
        rule(r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", "aws-key", false, 0),
        rule(
            r#"(?i)\b(?:aws_secret_access_key|secret_access_key)\b\s*[=:]\s*['"]?([A-Za-z0-9/+=]{40})\b"#,
            "aws-secret",
            false,
            1,
        ),
        rule(r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36}\b", "github-token", false, 0),
        rule(r"\bgithub_pat_[A-Za-z0-9_]{22,}", "github-token", false, 0),
        rule(r"\bxox[baprs]-[A-Za-z0-9-]{10,}", "slack-token", false, 0),
        rule(r"\bAIza[0-9A-Za-z_-]{35}", "google-key", false, 0),
        rule(r"\b[rs]k_(?:live|test)_[A-Za-z0-9]{24,}", "stripe-key", false, 0),
        rule(
            r"\beyJ[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}",
            "jwt",
            false,
            0,
        ),
        rule(r"(?i)\bbearer\s+([A-Za-z0-9\-._~+/=]{20,})", "bearer-token", false, 1),
        // `api_key = <value>`: only long values with a digit, so prose survives.
        rule(
            r#"(?i)\b(?:api[_-]?key|secret|token)\b\s*[=:]\s*['"]?([A-Za-z0-9_\-]{32,})"#,
            "secret",
            true,
            1,
        ),
    ]
});

/// Seed-phrase labels; a word run counts only if one sits within `LABEL_REACH`.
static MNEMONIC_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)mnemonic|seed\s+phrase|recovery\s+phrase|secret\s+phrase")
        .expect("label pattern compiles")
});
static WORDS_24: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:[a-z]{3,8}\s+){23}[a-z]{3,8}\b").expect("24-word pattern compiles")
});
static WORDS_12: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:[a-z]{3,8}\s+){11}[a-z]{3,8}\b").expect("12-word pattern compiles")
});
const LABEL_REACH: usize = 80;

/// Replace known secret shapes with `[REDACTED:<kind>]`.
pub fn scrub_secrets(text: &str) -> String {
    let mut out = text.to_string();
    for r in RULES.iter() {
        if !r.re.is_match(&out) {
            continue;
        }
        out = r
            .re
            .replace_all(&out, |c: &Captures| redact(c, r))
            .into_owned();
    }
    scrub_mnemonics(&out)
}

fn redact(c: &Captures, r: &Rule) -> String {
    let whole = c.get(0).expect("match");
    let secret = c.get(r.group).unwrap_or(whole);
    if r.needs_digit && !secret.as_str().bytes().any(|b| b.is_ascii_digit()) {
        return whole.as_str().to_string();
    }
    // Keep any label around the secret (e.g. `api_key = `).
    let start = secret.start() - whole.start();
    let end = secret.end() - whole.start();
    let w = whole.as_str();
    format!("{}[REDACTED:{}]{}", &w[..start], r.label, &w[end..])
}

/// Redact 24- and 12-word lowercase runs that start within `LABEL_REACH`
/// after a seed-phrase label, or end within it before one. Each label claims
/// at most one run, the search is anchored outside the label (so its own words
/// never join the run), and all spans come from the original text.
fn scrub_mnemonics(text: &str) -> String {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for label in MNEMONIC_LABEL.find_iter(text) {
        let (ls, le) = (label.start(), label.end());
        let after = [&*WORDS_24, &*WORDS_12].into_iter().find_map(|re| {
            re.find(&text[le..])
                .filter(|m| m.start() <= LABEL_REACH)
                .map(|m| (le + m.start(), le + m.end()))
        });
        let before = || {
            [&*WORDS_24, &*WORDS_12].into_iter().find_map(|re| {
                re.find_iter(&text[..ls])
                    .last()
                    .filter(|m| ls - m.end() <= LABEL_REACH)
                    .map(|m| (m.start(), m.end()))
            })
        };
        if let Some(span) = after.or_else(before) {
            if !spans.iter().any(|&(s, e)| span.0 < e && s < span.1) {
                spans.push(span);
            }
        }
    }
    spans.sort();
    let mut out = text.to_string();
    for (s, e) in spans.into_iter().rev() {
        out.replace_range(s..e, "[REDACTED:seed-phrase]");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keys are assembled at runtime so no realistic secret sits in the source.
    fn k(prefix: &str, body: &str, n: usize) -> String {
        let mut s = String::from(prefix);
        while s.len() < prefix.len() + n {
            s.push_str(body);
        }
        s.truncate(prefix.len() + n);
        s
    }

    fn redacted(text: &str, label: &str) -> bool {
        scrub_secrets(text).contains(&format!("[REDACTED:{label}]"))
    }

    fn unchanged(text: &str) -> bool {
        scrub_secrets(text) == text
    }

    #[test]
    fn hyphenated_vendor_keys_are_caught() {
        let ant = k(&["sk", "ant", "api03-"].join("-"), "Ab3_x-", 40);
        assert!(redacted(&format!("key={ant}"), "anthropic-key"));
        assert!(!scrub_secrets(&ant).contains(&ant[..20]));

        let proj = k(&["sk", "proj-"].join("-"), "Zx9_", 48);
        assert!(redacted(&proj, "openai-key"));
        assert!(unchanged("sk-proj-short"));
        assert!(unchanged("see sk-project-architecture-overview-and-notes"));
        assert!(unchanged("sk-ant-api-docs-are-long-enough-to-read"));

        let or = k(&["sk", "or", "v1-"].join("-"), "a1B2", 64);
        assert!(redacted(&or, "openrouter-key"));
        assert!(unchanged("sk-or-version-1-of-the-spec"));
    }

    #[test]
    fn plain_sk_keys_need_length_and_a_digit() {
        assert!(redacted(&k("sk-", "a1b2c3", 48), "api-key"));
        assert!(redacted(&k("sk-", "0f9e", 32), "api-key"));
        assert!(unchanged("sk-abcdefghijklmnopqrst"));
        assert!(unchanged(&k("sk-", "abcdef", 40)), "no digit: a word, not a key");
    }

    #[test]
    fn aws_ids_and_secrets() {
        assert!(redacted("My key is AKIAIOSFODNN7EXAMPLE", "aws-key"));
        assert!(redacted(&k("ASIA", "Q7", 16), "aws-key"));
        assert!(unchanged("AKIAiosfodnn7example"));
        assert!(unchanged(&k("AKIA", "Q7", 15)));
        let line = format!("aws_secret_access_key = {}", k("", "wJalrXUtnFEMI/K7MDENG+bPx", 40));
        let out = scrub_secrets(&line);
        assert!(out.starts_with("aws_secret_access_key = [REDACTED:aws-secret]"), "{out}");
        let sha = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
        assert!(unchanged(sha), "bare hashes survive");
    }

    #[test]
    fn github_slack_google_stripe_jwt() {
        assert!(redacted(&format!("Token: {}", k("ghp_", "1234567890abcdef", 36)), "github-token"));
        assert!(redacted(&k("ghs_", "Ab12", 36), "github-token"));
        assert!(unchanged("ghs_short"));
        assert!(redacted(&k("github_pat_", "11AB_", 40), "github-token"));
        assert!(redacted(&k(&["xox", "b-"].concat(), "1234567890-", 30), "slack-token"));
        assert!(unchanged("xbox-live-account"));
        assert!(redacted(&k("AIza", "Sy9_-", 35), "google-key"));
        assert!(unchanged(&k("AIza", "Sy9", 10)));
        assert!(redacted(&k(&["sk", "live", ""].join("_"), "a1B2", 24), "stripe-key"));
        assert!(unchanged("sk_live_short"));
        let jwt = format!("{}.{}.{}", k("eyJ", "hbGciOiJ", 30), k("", "eyJzdWIi", 20), k("", "SflKxw_", 20));
        assert!(redacted(&jwt, "jwt"));
        assert!(unchanged("eyJ.a.b"));
    }

    #[test]
    fn pem_blocks_of_every_kind() {
        for kind in ["", "RSA ", "EC ", "OPENSSH ", "ENCRYPTED "] {
            let pem = format!("-----BEGIN {kind}PRIVATE KEY-----\nMIIabc\n-----END {kind}PRIVATE KEY-----");
            assert!(redacted(&pem, "private-key"), "{kind}");
        }
        assert!(unchanged("the BEGIN PRIVATE KEY format is PEM"));
    }

    #[test]
    fn bearer_and_labelled_values_keep_their_label() {
        let out = scrub_secrets(&format!("Authorization: Bearer {}", k("", "abc123XYZ", 40)));
        assert!(out.starts_with("Authorization: Bearer [REDACTED:bearer-token]"), "{out}");
        let out = scrub_secrets(&format!("api_key = {}", k("", "q9w8e7r6t5", 32)));
        assert_eq!(out, "api_key = [REDACTED:secret]");
        assert!(unchanged("api_key = please-set-this-in-your-environment"));
    }

    #[test]
    fn mnemonics_only_near_a_label() {
        let seed = "abandon ability able about above absent absorb abstract absurd abuse access accident";
        let out = scrub_secrets(&format!("recovery phrase: {seed}"));
        assert_eq!(out, "recovery phrase: [REDACTED:seed-phrase]");

        let words24 = format!("{seed} {seed}");
        let out = scrub_secrets(&format!("seed phrase {words24}"));
        assert_eq!(out, "seed phrase [REDACTED:seed-phrase]", "24 words become one redaction: {out}");

        // A label far away does not reach an ordinary sentence.
        let far = format!(
            "recovery phrase docs. {} then later: {seed}",
            "x".repeat(200)
        );
        assert!(scrub_secrets(&far).contains(seed));
        // No label at all: kept.
        assert!(unchanged(seed));

        // A redaction must not act as a label for the prose after it.
        let prose = "then the team went over the plan again with fresh eyes and more coffee";
        let out = scrub_secrets(&format!("mnemonic: {seed}. {prose}"));
        assert!(out.ends_with(prose), "{out}");
    }

    #[test]
    fn clean_text_untouched() {
        assert!(unchanged("Hello, this is a normal message with no secrets."));
        assert!(unchanged("commit 3f5a794 fixed the build; see token budget of 2000"));
    }
}
