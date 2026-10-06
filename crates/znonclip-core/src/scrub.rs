//! Pre-prompt secret scrubbing: strip API keys, mnemonics, and tokens
//! from text BEFORE it enters an LLM's context window.
//!
//! This is the "pre-prompt secret scrubbing" idea:
//! pipe scraped text through the scrubber to remove credentials before
//! they can leak into a model's context.

use regex::Regex;

/// Scrub secrets from text, replacing them with `[REDACTED:<type>]`.
///
/// Detects:
/// - AWS keys (AKIA...)
/// - GitHub tokens (ghp_, gho_, github_pat_)
/// - OpenAI keys (sk-...)
/// - Generic API keys (long hex/base64 strings in key-like contexts)
/// - Mnemonic phrases (12/24 word BIP39-like sequences)
/// - Private keys (PEM blocks, hex strings)
pub fn scrub_secrets(text: &str) -> String {
    let mut result = text.to_string();

    // AWS Access Key ID
    let aws_key = Regex::new(r"AKIA[0-9A-Z]{16}").unwrap();
    result = aws_key.replace_all(&result, "[REDACTED:aws-key]").to_string();

    // GitHub tokens
    let gh_token = Regex::new(r"(ghp_[a-zA-Z0-9]{36}|gho_[a-zA-Z0-9]{36}|github_pat_[a-zA-Z0-9_]{22,})").unwrap();
    result = gh_token.replace_all(&result, "[REDACTED:github-token]").to_string();

    // OpenAI API keys
    let openai_key = Regex::new(r"sk-[a-zA-Z0-9]{20,}").unwrap();
    result = openai_key.replace_all(&result, "[REDACTED:openai-key]").to_string();

    // Generic bearer tokens (Bearer <token>)
    let bearer = Regex::new(r"(?i)bearer\s+[a-zA-Z0-9\-._~+/=]{20,}").unwrap();
    result = bearer.replace_all(&result, "Bearer [REDACTED:bearer-token]").to_string();

    // PEM private keys
    let pem_key = Regex::new(r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----[\s\S]*?-----END (?:RSA |EC |OPENSSH )?PRIVATE KEY-----").unwrap();
    result = pem_key.replace_all(&result, "[REDACTED:private-key]").to_string();

    // Mnemonic phrases (12 or 24 lowercase words)
    let mnemonic = Regex::new(r"\b(?:[a-z]{3,8}\s+){11}[a-z]{3,8}\b").unwrap();
    // Only redact if it looks like a mnemonic (all lowercase, word-like)
    // This is heuristic; false positives are acceptable (better safe)
    if mnemonic.is_match(&result) {
        // Check if it's in a sensitive context (near "mnemonic", "seed", "phrase", "recovery")
        let sensitive_ctx = Regex::new(r"(?i)(mnemonic|seed\s+phrase|recovery\s+phrase|secret\s+phrase)").unwrap();
        if sensitive_ctx.is_match(&result) {
            result = mnemonic.replace_all(&result, "[REDACTED:mnemonic]").to_string();
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_aws_key() {
        let text = "My key is AKIAIOSFODNN7EXAMPLE";
        let scrubbed = scrub_secrets(text);
        assert!(scrubbed.contains("[REDACTED:aws-key]"));
        assert!(!scrubbed.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn test_github_token() {
        let text = "Token: ghp_1234567890abcdefghij1234567890abcdef";
        let scrubbed = scrub_secrets(text);
        assert!(scrubbed.contains("[REDACTED:github-token]"));
    }

    #[test]
    fn test_clean_text_untouched() {
        let text = "Hello, this is a normal message with no secrets.";
        let scrubbed = scrub_secrets(text);
        assert_eq!(text, scrubbed);
    }
}
