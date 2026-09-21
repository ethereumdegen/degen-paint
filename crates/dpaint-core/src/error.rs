//! Structured errors. Every failure maps to a stable machine code and an exit code,
//! so an agent branches on `code`, never on message text. See `docs/errors.md`.

use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("selector '{selector}' matched 0 objects in {doc}")]
    SelectorNoMatch {
        selector: String,
        doc: String,
        candidates: Vec<String>,
    },
    #[error("selector '{selector}' matched {count} objects but exactly one is required")]
    SelectorAmbiguous {
        selector: String,
        count: usize,
        matches: Vec<String>,
    },
    #[error("unknown op '{0}'")]
    UnknownOp(String),
    #[error("invalid selector syntax: {0}")]
    SelectorSyntax(String),
    #[error("invalid arguments for '{op}': {detail}")]
    SchemaViolation { op: String, detail: String },
    #[error("op '{op}' cannot run on a {kind} document")]
    WrongDocumentKind { op: String, kind: String },
    #[error("document '{0}' not found")]
    NoSuchDocument(String),
    #[error("linking '{from}' to '{to}' would create a cycle")]
    CyclicLink { from: String, to: String },
    #[error("asset {0} is missing from the store")]
    AssetMissing(String),
    #[error("could not decode asset: {0}")]
    AssetDecode(String),
    #[error("font '{0}' is unavailable and no fallback was found")]
    FontUnavailable(String),
    #[error("degenerate geometry: {0}")]
    DegenerateGeometry(String),
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),
    #[error("project is locked by another writer (pid {0})")]
    ProjectLocked(String),
    #[error("project format v{found} requires migration (this build supports v{supported})")]
    MigrationRequired { found: u32, supported: u32 },
    #[error("provider '{0}' is not configured; set its API key")]
    ProviderUnconfigured(String),
    #[error("provider '{provider}' error: {detail}")]
    ProviderError { provider: String, detail: String },
    #[error("budget exceeded: {spent:.4} USD spent of {ceiling:.4} USD ceiling")]
    BudgetExceeded { spent: f64, ceiling: f64 },
    #[error("{0}")]
    Exists(String),
    #[error("{0}")]
    Invalid(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// Stable machine-readable code. Agents branch on this.
    pub fn code(&self) -> &'static str {
        use Error::*;
        match self {
            SelectorNoMatch { .. } => "selector_no_match",
            SelectorAmbiguous { .. } => "selector_ambiguous",
            UnknownOp(_) => "unknown_op",
            SelectorSyntax(_) => "selector_syntax",
            SchemaViolation { .. } => "schema_violation",
            WrongDocumentKind { .. } => "wrong_document_kind",
            NoSuchDocument(_) => "no_such_document",
            CyclicLink { .. } => "cyclic_link",
            AssetMissing(_) => "asset_missing",
            AssetDecode(_) => "asset_decode_failed",
            FontUnavailable(_) => "font_unavailable",
            DegenerateGeometry(_) => "degenerate_geometry",
            UnsupportedFormat(_) => "unsupported_format",
            ProjectLocked(_) => "project_locked",
            MigrationRequired { .. } => "migration_required",
            ProviderUnconfigured(_) => "provider_unconfigured",
            ProviderError { .. } => "provider_error",
            BudgetExceeded { .. } => "budget_exceeded",
            Exists(_) => "exists",
            Invalid(_) => "invalid",
            Io(_) => "io_error",
            Json(_) => "json_error",
        }
    }

    /// Process exit code. See the table in `docs/errors.md`.
    pub fn exit_code(&self) -> i32 {
        use Error::*;
        match self {
            SchemaViolation { .. }
            | UnknownOp(_)
            | SelectorSyntax(_)
            | WrongDocumentKind { .. } => 2,
            SelectorNoMatch { .. } | SelectorAmbiguous { .. } => 3,
            ProviderUnconfigured(_) | ProviderError { .. } => 5,
            BudgetExceeded { .. } => 6,
            ProjectLocked(_) => 7,
            _ => 1,
        }
    }

    /// Extra context an agent can act on without another round trip.
    pub fn detail(&self) -> ErrorDetail {
        let mut d = ErrorDetail {
            code: self.code(),
            message: self.to_string(),
            candidates: Vec::new(),
            suggestion: None,
        };
        match self {
            Error::SelectorNoMatch {
                selector,
                candidates,
                ..
            } => {
                d.suggestion = nearest(selector, candidates);
                d.candidates = candidates.clone();
            }
            Error::SelectorAmbiguous { matches, .. } => d.candidates = matches.clone(),
            _ => {}
        }
        d
    }
}

#[derive(Debug, Serialize)]
pub struct ErrorDetail {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

/// Cheap edit-distance suggestion so a typo costs zero extra turns.
fn nearest(needle: &str, hay: &[String]) -> Option<String> {
    let n = needle.trim_start_matches(['#', '@']);
    if n.is_empty() {
        return None;
    }
    // A candidate that contains what was asked for is a better guess than one a few
    // edits away: '#sky' means '#sky-grad', not '#bg'.
    let limit = (n.chars().count() / 2).max(2);
    hay.iter()
        .filter_map(|c| {
            let bare = c.trim_start_matches(['#', '@']);
            let score = if bare == n {
                0
            } else if bare.contains(n) || n.contains(bare) {
                1
            } else {
                let d = levenshtein(n, bare);
                if d > limit {
                    return None;
                }
                d + 1
            };
            Some((score, c))
        })
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c.clone())
}

fn levenshtein(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_the_near_miss_and_keeps_codes_stable() {
        let e = Error::SelectorNoMatch {
            selector: "#sky".into(),
            doc: "doc_main".into(),
            candidates: vec!["#bg".into(), "#sky-grad".into(), "#title".into()],
        };
        let d = e.detail();
        assert_eq!(d.code, "selector_no_match");
        assert_eq!(d.suggestion.as_deref(), Some("#sky-grad"));
        assert_eq!(e.exit_code(), 3);
    }

    #[test]
    fn unrelated_candidates_produce_no_suggestion() {
        let e = Error::SelectorNoMatch {
            selector: "#zzzzzzzz".into(),
            doc: "doc_main".into(),
            candidates: vec!["#bg".into()],
        };
        assert_eq!(e.detail().suggestion, None);
    }
}
