//! The recorded-response fixture transport (docs/implementation/13 §M3
//! step 5: "replays recorded responses instead of network").
//!
//! A fixture directory carries one `manifest.json`: an array of recorded
//! responses, each with the exact URL, the status, response headers, and
//! the UTF-8 body. Replay matches the URL exactly; a miss fails closed.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::IngestError;

/// One recorded response in a fixture manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedResponse {
    /// The exact request URL the response answers.
    pub url: String,
    /// The HTTP status of the recording.
    pub status: u16,
    /// Response headers, lower-cased names.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// The UTF-8 body.
    pub body: String,
}

/// Header names of a recorded response, lower-cased keys.
type BTreeMap<K, V> = std::collections::BTreeMap<K, V>;

/// The fixture set: recorded responses keyed by exact URL.
#[derive(Debug, Clone, Default)]
pub struct FixtureSet {
    responses: HashMap<String, RecordedResponse>,
}

impl FixtureSet {
    /// Loads `dir/manifest.json`.
    ///
    /// # Errors
    /// [`IngestError::Fixture`] when the manifest is unreadable or invalid.
    pub fn load(dir: &Path) -> Result<Self, IngestError> {
        let manifest = dir.join("manifest.json");
        let text = std::fs::read_to_string(&manifest).map_err(|e| IngestError::Fixture {
            path: manifest.display().to_string(),
            message: e.to_string(),
        })?;
        Self::from_json(&text)
    }

    /// Parses the manifest JSON.
    fn from_json(text: &str) -> Result<Self, IngestError> {
        let responses: Vec<RecordedResponse> =
            serde_json::from_str(text).map_err(|e| IngestError::Fixture {
                path: String::from("manifest.json"),
                message: e.to_string(),
            })?;
        let map = responses
            .into_iter()
            .map(|response| (response.url.clone(), response))
            .collect();
        Ok(Self { responses: map })
    }

    /// The recorded response for `url`, if one was recorded.
    #[must_use]
    pub fn serve(&self, url: &Url) -> Option<&RecordedResponse> {
        self.responses.get(url.as_str())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::FixtureSet;
    use crate::error::IngestError;
    use url::Url;

    /// A valid manifest loads and serves by exact URL.
    #[test]
    fn loads_and_serves_by_exact_url() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("manifest.json");
        std::fs::write(
            &manifest,
            r#"[{"url": "https://docs.acme.test/llms.txt", "status": 200, "body": "index"}]"#,
        )
        .unwrap();
        let set = FixtureSet::load(dir.path()).unwrap();
        let hit = set
            .serve(&Url::parse("https://docs.acme.test/llms.txt").unwrap())
            .unwrap();
        assert_eq!(hit.body, "index");
        assert!(
            set.serve(&Url::parse("https://docs.acme.test/missing.md").unwrap())
                .is_none()
        );
    }

    /// A missing manifest and a broken manifest fail with the fixture
    /// error.
    #[test]
    fn bad_manifests_fail() {
        let dir = tempfile::tempdir().unwrap();
        let error = FixtureSet::load(dir.path()).unwrap_err();
        assert!(matches!(error, IngestError::Fixture { .. }));

        let manifest = dir.path().join("manifest.json");
        std::fs::write(&manifest, "not json").unwrap();
        let error = FixtureSet::load(dir.path()).unwrap_err();
        assert!(matches!(error, IngestError::Fixture { .. }));
    }
}
