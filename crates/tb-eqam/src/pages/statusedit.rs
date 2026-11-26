//! `statusedit.cgi`: the 16-row stream table form.

use std::collections::BTreeSet;

use crate::html::{Document, Form};
use crate::pages::{ParseError, expect_page};
use crate::types::{Slot, StreamName, StreamUrl};

const PAGE: &str = "statusedit.cgi";

/// One row of the form as rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditRow {
    /// Slot.
    pub slot: Slot,
    /// `streamGroupAddress<n>` as rendered.
    pub url_text: String,
    /// `streamName<n>` as rendered.
    pub name_text: String,
    /// `proIdiomEnable<n>` checked.
    pub pro_idiom: bool,
}

impl EditRow {
    /// The URL, when the text is one this crate would also write.
    pub fn url(&self) -> Option<StreamUrl> {
        self.url_text.parse().ok()
    }

    /// The name, when the text is a valid name.
    pub fn name(&self) -> Option<StreamName> {
        self.name_text.parse().ok()
    }
}

/// The parsed form, kept whole so a write can post every field back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEditPage {
    form: Form,
    /// The 16 rows in slot order.
    pub rows: Vec<EditRow>,
}

/// One field's signature: name, control kind, and whether it is rendered disabled
/// (`None` = either is accepted).
type FieldSig = (String, &'static str, Option<bool>);

/// The field set the 1.1.13 page renders in MPTS mode with IGMPv2 (device §3): per slot
/// `streamGroupAddress<n>` (text), `streamSourceAddress<n>A/B` (text, disabled), `streamName<n>`
/// (text), `proIdiomEnable<n>` (checkbox); then `session_id` (hidden) and `btnSave` (submit).
pub fn expected_fields() -> BTreeSet<FieldSig> {
    let mut s = BTreeSet::new();
    for n in 1..=16 {
        s.insert((format!("streamGroupAddress{n}"), "text", Some(false)));
        s.insert((format!("streamSourceAddress{n}A"), "text", Some(true)));
        s.insert((format!("streamSourceAddress{n}B"), "text", Some(true)));
        s.insert((format!("streamName{n}"), "text", Some(false)));
        // The page's script may disable unchecked boxes; the server renders them enabled.
        s.insert((format!("proIdiomEnable{n}"), "checkbox", None));
    }
    s.insert((String::from("session_id"), "hidden", Some(false)));
    s.insert((String::from("btnSave"), "submit", Some(false)));
    s
}

/// The form's fields differ from the expected set: the write is aborted (SPEC §10.1).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{form}: unexpected field set: missing {missing:?}, extra {extra:?}")]
pub struct FieldSetMismatch {
    /// Form.
    pub form: &'static str,
    /// Expected but absent (or with another kind or disabled state).
    pub missing: Vec<String>,
    /// Present but not expected.
    pub extra: Vec<String>,
}

/// Compare a form's controls with an expected signature set.
pub(crate) fn check_field_set(
    form: &Form,
    expected: &BTreeSet<FieldSig>,
    name: &'static str,
) -> Result<(), FieldSetMismatch> {
    let mut missing = Vec::new();
    let mut extra = Vec::new();
    let mut seen = BTreeSet::new();
    for c in &form.controls {
        if !seen.insert(c.name.clone()) {
            extra.push(format!("{} (twice)", c.name));
            continue;
        }
        let ok = expected.iter().any(|(n, k, d)| {
            *n == c.name && *k == c.kind.tag() && d.is_none_or(|d| d == c.disabled)
        });
        if !ok {
            extra.push(format!(
                "{} ({}{})",
                c.name,
                c.kind.tag(),
                if c.disabled { ", disabled" } else { "" }
            ));
        }
    }
    for (n, k, d) in expected {
        let present = form
            .controls
            .iter()
            .any(|c| c.name == *n && c.kind.tag() == *k && d.is_none_or(|d| d == c.disabled));
        if !present {
            missing.push(format!("{n} ({k})"));
        }
    }
    if missing.is_empty() && extra.is_empty() {
        Ok(())
    } else {
        Err(FieldSetMismatch {
            form: name,
            missing,
            extra,
        })
    }
}

impl StatusEditPage {
    /// Parse the page body.
    pub fn parse(body: &[u8]) -> Result<Self, ParseError> {
        let doc = Document::parse(body);
        expect_page(&doc, "Status Edit")?;
        let form = doc
            .forms()
            .into_iter()
            .find(|f| f.name.as_deref() == Some("configForm"))
            .ok_or_else(|| ParseError::structure(PAGE, "configForm missing"))?;
        let mut rows = Vec::with_capacity(16);
        for slot in Slot::all() {
            let n = slot.get();
            let text = |k: String| {
                form.control(&k)
                    .and_then(|c| c.text_value())
                    .map(str::to_owned)
                    .ok_or_else(|| ParseError::structure(PAGE, format!("{k} missing")))
            };
            rows.push(EditRow {
                slot,
                url_text: text(format!("streamGroupAddress{n}"))?,
                name_text: text(format!("streamName{n}"))?,
                pro_idiom: form
                    .control(&format!("proIdiomEnable{n}"))
                    .is_some_and(|c| c.successful_value().is_some()),
            });
        }
        Ok(StatusEditPage { form, rows })
    }

    /// Check the form has exactly the expected fields.
    pub fn check_fields(&self) -> Result<(), FieldSetMismatch> {
        check_field_set(&self.form, &expected_fields(), PAGE)
    }

    /// The form as parsed.
    pub fn form(&self) -> &Form {
        &self.form
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::fixtures;

    #[test]
    fn golden_saved_statusedit_page() {
        let p = StatusEditPage::parse(fixtures::STATUS_EDIT).unwrap();
        assert_eq!(p.rows.len(), 16);
        for (r, s) in p.rows.iter().zip(Slot::all()) {
            assert_eq!(r.slot, s);
            assert_eq!(r.url_text, "");
            assert_eq!(r.name_text, "");
            assert!(!r.pro_idiom);
        }
        p.check_fields().unwrap();
        assert_eq!(p.form().action.as_deref(), Some("statusedit.cgi"));
        assert_eq!(p.form().method.as_deref(), Some("post"));
        assert_eq!(p.form().controls.len(), 16 * 5 + 2);
    }

    #[test]
    fn an_enabled_source_field_aborts() {
        // IGMPv3 mode would enable the SSM source fields: a different form, so no write.
        let s = String::from_utf8_lossy(fixtures::STATUS_EDIT).replace(
            "<input disabled  type=\"text\" name=\"streamSourceAddress3A\"",
            "<input   type=\"text\" name=\"streamSourceAddress3A\"",
        );
        let p = StatusEditPage::parse(s.as_bytes()).unwrap();
        let e = p.check_fields().unwrap_err();
        assert!(
            e.extra
                .iter()
                .any(|x| x.starts_with("streamSourceAddress3A"))
        );
        assert!(
            e.missing
                .iter()
                .any(|x| x.starts_with("streamSourceAddress3A"))
        );
    }

    #[test]
    fn a_new_field_aborts() {
        let s = String::from_utf8_lossy(fixtures::STATUS_EDIT).replace(
            "<input name=\"session_id\" value=\"\" type=\"hidden\">",
            "<input name=\"session_id\" value=\"\" type=\"hidden\"><input name=\"programNumber1\" value=\"1\">",
        );
        let e = StatusEditPage::parse(s.as_bytes())
            .unwrap()
            .check_fields()
            .unwrap_err();
        assert_eq!(e.extra, vec![String::from("programNumber1 (text)")]);
        // And a missing one.
        let s = String::from_utf8_lossy(fixtures::STATUS_EDIT).replace(
            "<input value=\"Save\" style=\"width: 85px;\" name=\"btnSave\" type=\"submit\">",
            "",
        );
        let e = StatusEditPage::parse(s.as_bytes())
            .unwrap()
            .check_fields()
            .unwrap_err();
        assert_eq!(e.missing, vec![String::from("btnSave (submit)")]);
    }
}
