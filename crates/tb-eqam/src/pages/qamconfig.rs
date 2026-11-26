//! `qamconfig.cgi?slotNumber=<m>`: one RF module's settings.

use std::collections::BTreeSet;

use crate::html::{Document, Form, row_cells};
use crate::pages::statusedit::{FieldSetMismatch, check_field_set};
use crate::pages::{PageHeader, ParseError, expect_page};
use crate::plan::ModulePlan;
use crate::types::{Channel, FirstChannel, OutputLevel, QamMap, QamMode, RfModule};

const PAGE: &str = "qamconfig.cgi";

/// `QAM Lock State`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockState {
    /// `Lock`.
    Lock,
    /// `Unlock`.
    Unlock,
    /// Anything else, verbatim.
    Other(String),
}

/// One module's page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QamConfigPage {
    form: Form,
    /// Page header.
    pub header: PageHeader,
    /// The module the page is for (from the title and the selected `slotNumber`).
    pub module: RfModule,
    /// Selected channel of carriers A–D (B–D are the disabled, derived selects).
    pub channels: [Option<Channel>; 4],
    /// Carrier A–D on.
    pub carriers_on: [bool; 4],
    /// "Enable CW for QAM Module" checked.
    pub cw: bool,
    /// Selected output level.
    pub level: Option<OutputLevel>,
    /// Whether the level select is enabled (module 1 only).
    pub level_editable: bool,
    /// QAM mode.
    pub qam: Option<QamMode>,
    /// Channel plan.
    pub map: Option<QamMap>,
    /// `Output QAM Data Rate`, verbatim (e.g. `5.360500 Mbaud`).
    pub data_rate: String,
    /// `Output QAM Interleaver`, verbatim.
    pub interleaver: String,
    /// `Output QAM Alpha`, verbatim.
    pub alpha: String,
    /// `QAM Lock State`.
    pub lock: LockState,
    /// `Other Errors`, when the row is present.
    pub other_errors: Option<String>,
    /// The page says "There was an error communicating with the QAM Module".
    pub communication_error: bool,
    /// The values `outputChannelA` offers.
    pub first_channel_options: Vec<u8>,
}

/// The field set of a module page (device §3): `slotNumber`, `outputChannelA` (enabled),
/// `outputChannelB..D` (disabled), `outputChannelControlA..D`, `chkCW1`, `outputLevel`
/// (enabled on module 1 only), `QAMModeA`, `QAMMap`, `btnQAMSave` (button), `qam_saved` and
/// `session_id` (hidden).
pub fn expected_fields(module: RfModule) -> BTreeSet<(String, &'static str, Option<bool>)> {
    let mut s = BTreeSet::new();
    let mut add = |n: &str, k: &'static str, d: bool| {
        s.insert((n.to_owned(), k, Some(d)));
    };
    add("slotNumber", "select", false);
    add("outputChannelA", "select", false);
    add("outputChannelB", "select", true);
    add("outputChannelC", "select", true);
    add("outputChannelD", "select", true);
    add("outputChannelControlA", "select", false);
    add("outputChannelControlB", "select", false);
    add("outputChannelControlC", "select", false);
    add("outputChannelControlD", "select", false);
    add("chkCW1", "checkbox", false);
    add("outputLevel", "select", module.get() != 1);
    add("QAMModeA", "select", false);
    add("QAMMap", "select", false);
    add("btnQAMSave", "button", false);
    add("qam_saved", "hidden", false);
    add("session_id", "hidden", false);
    s
}

impl QamConfigPage {
    /// Parse the page body.
    pub fn parse(body: &[u8]) -> Result<Self, ParseError> {
        let doc = Document::parse(body);
        expect_page(&doc, "QAM Config")?;
        let header = PageHeader::parse(&doc, PAGE)?;
        let title = doc.title().unwrap_or_default();
        let title_module = title
            .strip_prefix("QAM Config - RF Module ")
            .and_then(|r| r.split(' ').next())
            .and_then(|n| n.parse::<u8>().ok())
            .and_then(|n| RfModule::new(n).ok())
            .ok_or_else(|| ParseError::structure(PAGE, format!("title {title:?}")))?;
        let form = doc
            .forms()
            .into_iter()
            .find(|f| f.name.as_deref() == Some("configForm"))
            .ok_or_else(|| ParseError::structure(PAGE, "configForm missing"))?;
        let selected_u8 =
            |name: &str| -> Option<u8> { form.control(name)?.selected()?.parse().ok() };
        let slot_sel = selected_u8("slotNumber");
        if slot_sel != Some(title_module.get()) {
            return Err(ParseError::structure(
                PAGE,
                format!("title says module {title_module}, slotNumber selects {slot_sel:?}"),
            ));
        }
        let channels = ["A", "B", "C", "D"]
            .map(|l| selected_u8(&format!("outputChannel{l}")).and_then(|c| Channel::new(c).ok()));
        let carriers_on = ["A", "B", "C", "D"]
            .map(|l| selected_u8(&format!("outputChannelControl{l}")) == Some(1));
        let cw = form.control("chkCW1").is_some_and(|c| {
            matches!(
                c.kind,
                crate::html::ControlKind::Checkbox { checked: true, .. }
            )
        });
        let level_ctl = form.control("outputLevel");
        let level = selected_u8("outputLevel").and_then(|v| OutputLevel::new(v).ok());
        let level_editable = level_ctl.is_some_and(|c| !c.disabled);
        let qam = selected_u8("QAMModeA").and_then(|v| QamMode::from_form_value(v).ok());
        let map = selected_u8("QAMMap").and_then(|v| QamMap::from_form_value(v).ok());
        let first_channel_options = match form.control("outputChannelA").map(|c| &c.kind) {
            Some(crate::html::ControlKind::Select { options }) => options
                .iter()
                .filter_map(|o| o.value.parse().ok())
                .collect(),
            _ => Vec::new(),
        };
        let mut data_rate = String::new();
        let mut interleaver = String::new();
        let mut alpha = String::new();
        let mut lock = None;
        let mut other_errors = None;
        for tr in doc.find_all("tr") {
            let cells = row_cells(tr);
            if cells.len() < 2 {
                continue;
            }
            let v = cells[1].text();
            match cells[0].text().as_str() {
                "Output QAM Data Rate" => data_rate = v,
                "Output QAM Interleaver" => interleaver = v,
                "Output QAM Alpha" => alpha = v,
                "QAM Lock State" => {
                    lock = Some(match v.as_str() {
                        "Lock" => LockState::Lock,
                        "Unlock" => LockState::Unlock,
                        _ => LockState::Other(v),
                    });
                }
                "Other Errors" => other_errors = Some(v),
                _ => {}
            }
        }
        let lock = lock.ok_or_else(|| ParseError::structure(PAGE, "QAM Lock State row missing"))?;
        Ok(QamConfigPage {
            communication_error: doc
                .text()
                .contains("There was an error communicating with the QAM Module"),
            form,
            header,
            module: title_module,
            channels,
            carriers_on,
            cw,
            level,
            level_editable,
            qam,
            map,
            data_rate,
            interleaver,
            alpha,
            lock,
            other_errors,
            first_channel_options,
        })
    }

    /// Check the form has exactly the expected fields for its module.
    pub fn check_fields(&self) -> Result<(), FieldSetMismatch> {
        check_field_set(&self.form, &expected_fields(self.module), PAGE)
    }

    /// The form as parsed.
    pub fn form(&self) -> &Form {
        &self.form
    }

    /// The module's settings as a plan entry, when every value is readable and carriers B–D
    /// follow carrier A as the device's table says.
    pub fn as_module_plan(&self) -> Option<ModulePlan> {
        let a = self.channels[0]?;
        let first = FirstChannel::new(a.get()).ok()?;
        let derived = first.carriers();
        if self
            .channels
            .iter()
            .zip(derived)
            .any(|(o, d)| *o != Some(d))
        {
            return None;
        }
        Some(ModulePlan {
            first_channel: first,
            qam: self.qam?,
            map: self.map?,
            carriers_on: self.carriers_on,
        })
    }
}

/// The `cable_channel_table_after` array in the page's script (for checking our copy).
pub fn script_channel_table(body: &[u8]) -> Option<Vec<u8>> {
    let doc = Document::parse(body);
    doc.find_all("script").into_iter().find_map(|s| {
        let t = s.text();
        let start = t.find("cable_channel_table_after = new Array(")?
            + "cable_channel_table_after = new Array(".len();
        let end = t[start..].find(')')? + start;
        t[start..end]
            .split(',')
            .map(|v| v.trim().parse::<u8>().ok())
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::CABLE_CHANNEL_TABLE_AFTER;
    use crate::pages::fixtures;

    #[test]
    fn golden_saved_qam_pages() {
        let want = [
            [2, 3, 4, 5],
            [10, 11, 12, 13],
            [6, 95, 96, 97],
            [140, 141, 142, 143],
        ];
        for (i, body) in fixtures::QAM.iter().enumerate() {
            let p = QamConfigPage::parse(body).unwrap();
            let m = RfModule::new(u8::try_from(i + 1).unwrap()).unwrap();
            assert_eq!(p.module, m);
            assert_eq!(p.channels.map(|c| c.unwrap().get()), want[i]);
            assert_eq!(p.carriers_on, [true; 4]);
            assert!(!p.cw);
            assert_eq!(p.level, Some(OutputLevel::new(40).unwrap()));
            assert_eq!(p.level_editable, i == 0);
            assert_eq!(p.qam, Some(QamMode::Qam256B));
            assert_eq!(p.map, Some(QamMap::Std));
            assert_eq!(p.data_rate, "5.360500 Mbaud");
            assert_eq!(p.interleaver, "128-1");
            assert_eq!(p.alpha, "12%");
            assert_eq!(p.lock, LockState::Lock);
            assert_eq!(p.other_errors, None);
            assert!(!p.communication_error);
            assert_eq!(p.first_channel_options, (2..=155).collect::<Vec<u8>>());
            p.check_fields().unwrap();
            let plan = p.as_module_plan().unwrap();
            assert_eq!(plan.first_channel.channel().get(), want[i][0]);
        }
    }

    #[test]
    fn channel_table_matches_the_page() {
        for body in fixtures::QAM {
            assert_eq!(
                script_channel_table(body).unwrap(),
                CABLE_CHANNEL_TABLE_AFTER.to_vec()
            );
        }
    }

    #[test]
    fn page_of_another_module_or_level_state_is_caught() {
        // Module 2's form with the level select enabled is not module 2's expected form.
        let s = String::from_utf8_lossy(fixtures::QAM[1]).replace(
            "<select disabled size=\"1\" name=\"outputLevel\">",
            "<select  size=\"1\" name=\"outputLevel\">",
        );
        let p = QamConfigPage::parse(s.as_bytes()).unwrap();
        assert!(p.check_fields().is_err());
        // Title and selected slot disagree.
        let s = String::from_utf8_lossy(fixtures::QAM[1]).replace("RF Module 2 -", "RF Module 3 -");
        assert!(matches!(
            QamConfigPage::parse(s.as_bytes()),
            Err(ParseError::Structure { .. })
        ));
    }
}
