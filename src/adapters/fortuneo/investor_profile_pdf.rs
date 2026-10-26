//! Fortuneo Vie / PER: the "Profil investisseur et profil extra-financier" form, the result of the questionnaire the
//! client fills in before investing.
//!
//! A document to keep, not to import: it says which investor profile the insurer holds for the client (and since when),
//! which is what the advice given on the contract rests on. Only the result is read: the project, the investment
//! horizon, the two profiles and the day they were established. The answers to the questionnaire (family situation,
//! job, knowledge, savings...) are personal and stay in the file, they are not copied into the database.

use chrono::NaiveDate;

use crate::adapters::base::{round2, score, Adapter, Detection, ParseResult, ParsedStatement, SourceDocument};
use crate::adapters::common::{french_date, norm};
use crate::adapters::pdftools;
use crate::adapters::re::{search, sub};
use crate::py;

pub struct FortuneoInvestorProfilePdf;

/// "DYNAMIQUE" -> "Dynamique", "SANS PREFERENCE" -> "Sans préférence" (the form prints its results in capitals and
/// without accents).
fn sentence_case(text: &str) -> String {
    let lower = py::lower(py::strip(text));
    let fixed = lower.replace("preference", "préférence").replace("equilibre", "équilibré");
    py::capitalize(&fixed)
}

impl Adapter for FortuneoInvestorProfilePdf {
    fn id(&self) -> &'static str {
        "fortuneo.investor_profile_pdf"
    }
    fn institution(&self) -> Option<&'static str> {
        Some("Fortuneo")
    }
    fn kind(&self) -> &'static str {
        "info"
    }
    fn label(&self) -> &'static str {
        "Fortuneo Vie · profil investisseur (PDF)"
    }
    fn description(&self) -> &'static str {
        "Le formulaire « Profil investisseur et profil extra-financier » : projet, horizon de placement et profils établis à la date indiquée."
    }
    fn formats(&self) -> &'static [&'static str] {
        &["pdf"]
    }

    fn detect(&self, doc: &SourceDocument) -> Result<Detection, String> {
        if doc.format() != "pdf" {
            return Ok(Detection::default());
        }
        let c = sub(r"[^a-z0-9]", "", doc.compact()?);
        let (mut s, reasons) = score(&[
            (c.contains("fortuneo"), 0.2, "mentionne Fortuneo"),
            (c.contains("profilinvestisseuretprofilextrafinancier"), 0.4, "« Profil investisseur et profil extra-financier »"),
            (c.contains("vousavezunprofilinvestisseur"), 0.2, "« Vous avez un profil investisseur »"),
            (c.contains("votrehorizondeplacement"), 0.1, "« Votre horizon de placement »"),
            (c.contains("suravenir"), 0.1, "mentionne Suravenir"),
        ]);
        if !c.contains("fortuneo") {
            s = s.min(0.3);
        }
        Ok(Detection { score: round2(s.min(1.0)), reasons })
    }

    fn parse(&self, doc: &SourceDocument) -> ParseResult {
        let mut st = ParsedStatement::new("pdf");
        st.institution = Some("Fortuneo".into());
        st.adapter_id = Some(self.id().into());
        st.document_kind = Some(self.kind().into());
        st.confidence = "high".into();

        let mut texts: Vec<String> = vec![];
        for words in doc.words(1.0)?.iter() {
            for row in pdftools::lines(words, 3.0, false) {
                let kept: Vec<_> = row.into_iter().filter(|w| w.x0 >= 20.0).collect(); // the margin carries a rotated reference
                texts.push(py::strip(&pdftools::row_text(&kept)).to_string());
            }
        }
        // "VOTRE PROJET : faire fructifier votre argent": the label, then the answer
        let after = |label: &str| -> Option<String> {
            texts.iter().find_map(|t| {
                let n = norm(t);
                n.starts_with(label).then(|| t.split_once(':').map(|(_, v)| py::strip(v).to_string())).flatten().filter(|v| !v.is_empty())
            })
        };
        let project = after("votre projet");
        let horizon = after("votre horizon de placement");
        let investor = after("vous avez un profil investisseur");
        let extra_financial = after("vous avez un profil extra-financier");
        let established: Option<NaiveDate> = texts.iter().find_map(|t| {
            let n = norm(t);
            n.starts_with("ce profil a ete etabli").then(|| search(r"(\d{1,2}(?:er)?\s+[^\W\d_]+\s+\d{4})\s*$", t).and_then(|m| french_date(Some(&m[1])))).flatten()
        });

        let mut problems: Vec<String> = vec![];
        if let Some(v) = &project {
            st.set_extra("project", py::capitalize(v));
        }
        if let Some(v) = &horizon {
            st.set_extra("horizon", v.clone());
        }
        if let Some(v) = &investor {
            st.set_extra("investor_profile", sentence_case(v));
        } else {
            problems.push("Le profil investisseur n'a pas été trouvé".into());
        }
        if let Some(v) = &extra_financial {
            st.set_extra("extra_financial_profile", sentence_case(v));
        }
        match established {
            Some(d) => {
                st.balance_date = Some(d);
                st.set_extra("established_on", d.format("%Y-%m-%d").to_string());
            }
            None => problems.push("La date à laquelle le profil a été établi n'a pas été trouvée".into()),
        }

        // "Nom de naissance : X" / "Prénom(s) : Y": the person the profile is about
        let field = |label: &str| after(label);
        let (last, first) = (field("nom de naissance"), field("prenom"));
        if let (Some(l), Some(f)) = (&last, &first) {
            st.holder_name = Some(format!("{} {}", py::upper(l), py::upper(f)));
        }

        let title = match &investor {
            Some(v) => format!("Profil investisseur · {}", sentence_case(v)),
            None => "Profil investisseur".into(),
        };
        st.set_extra("title", title);
        st.set_extra("category", "Profil investisseur");
        let has_problems = !problems.is_empty();
        st.warnings.extend(problems);
        if has_problems {
            st.confidence = if investor.is_none() { "low".into() } else { "medium".into() };
        }
        Ok(vec![st])
    }
}
