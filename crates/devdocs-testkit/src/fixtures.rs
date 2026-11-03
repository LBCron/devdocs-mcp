//! The llms.txt fixture files (docs/implementation/04 §Fixtures).
//!
//! The samples are hand-written to the shapes the milestone needs: the
//! `llmstxt.org` example (`FastHTML`), a Mintlify-style index, and a valid
//! index with zero links. None of them copies content from a source with
//! an unclear license.

/// The `llmstxt.org`-shaped example index (`FastHTML` sample).
pub const LLMS_TXT_SPEC_EXAMPLE: &str = include_str!("../testdata/llms_txt/spec_example.txt");

/// A Mintlify-style index: H2 groups, one `.md` link per page.
pub const LLMS_TXT_MINTLIFY_STYLE: &str = include_str!("../testdata/llms_txt/mintlify_style.txt");

/// A valid index with zero links; the zero-links gate must refuse it.
pub const LLMS_TXT_NO_LINKS: &str = include_str!("../testdata/llms_txt/no_links.txt");
