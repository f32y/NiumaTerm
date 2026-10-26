mod choices;
mod elicitation;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashSet};

use serde_json::Value;

use crate::chat::{QuestionAnswer, QuestionMode, QuestionRequest};
use crate::claude_code::stream_json::dialogs::choices::{ChoiceDialog, SUPPORTED_KINDS};
use crate::claude_code::stream_json::dialogs::elicitation::Elicitation;

pub(super) fn supported_kinds() -> &'static [&'static str] {
    SUPPORTED_KINDS
}

enum Dialog {
    Choice(ChoiceDialog),
    Form(Elicitation),
}

impl Dialog {
    fn parse(request: &Value) -> Result<Option<Self>, String> {
        match request["subtype"].as_str() {
            Some("request_user_dialog") => {
                let kind = request["dialog_kind"]
                    .as_str()
                    .ok_or("Claude sent a dialog without a type.")?;

                ChoiceDialog::parse(kind, &request["payload"])
                    .map(|dialog| dialog.map(Self::Choice))
            }
            Some("elicitation") => Elicitation::parse(request).map(|form| Some(Self::Form(form))),
            _ => Ok(None),
        }
    }

    fn request(&self, id: String) -> QuestionRequest {
        let (mode, questions) = match self {
            Self::Choice(dialog) => (QuestionMode::Dialog, dialog.questions()),
            Self::Form(form) => (QuestionMode::Form, form.questions()),
        };

        QuestionRequest {
            id,
            mode,
            questions: questions.to_vec(),
        }
    }

    fn retracted_messages(&self) -> &[String] {
        match self {
            Self::Choice(dialog) => dialog.retracted_messages(),
            Self::Form(_) => &[],
        }
    }

    fn respond(&self, answers: Option<&[QuestionAnswer]>) -> Result<DialogReply, String> {
        let (response, edit_prompt) = match self {
            Self::Choice(dialog) => {
                let reply = dialog.respond(answers)?;

                (reply.response, reply.edit_prompt)
            }
            Self::Form(form) => (form.respond(answers)?, false),
        };

        Ok(DialogReply {
            response,
            edit_prompt,
            retracted_messages: self.retracted_messages().to_vec(),
        })
    }
}

pub(super) struct DialogReply {
    pub(super) response: Value,
    pub(super) edit_prompt: bool,
    pub(super) retracted_messages: Vec<String>,
}

/// Requests are retained until their response is accepted by the process writer.
#[derive(Default)]
pub(super) struct Dialogs {
    pending: BTreeMap<String, Dialog>,
    retired: HashSet<String>,
}

impl Dialogs {
    pub(super) fn receive(
        &mut self,
        id: &str,
        request: &Value,
    ) -> Result<Option<QuestionRequest>, String> {
        if id.is_empty() {
            return Err("Claude sent a dialog without a request ID.".into());
        }

        if self.pending.contains_key(id) || self.retired.contains(id) {
            return Ok(None);
        }

        let Some(dialog) = Dialog::parse(request)? else {
            return Err(format!(
                "This Claude dialog is not supported: {}.",
                request["dialog_kind"].as_str().unwrap_or("unknown")
            ));
        };

        let question = dialog.request(id.to_owned());

        self.pending.insert(id.to_owned(), dialog);

        Ok(Some(question))
    }

    pub(super) fn contains(&self, id: &str) -> bool {
        self.pending.contains_key(id)
    }

    pub(super) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub(super) fn respond(
        &self,
        id: &str,
        answers: Option<&[QuestionAnswer]>,
    ) -> Result<DialogReply, String> {
        self.pending
            .get(id)
            .ok_or("This dialog is no longer pending.")?
            .respond(answers)
    }

    pub(super) fn decline(&self, id: &str) -> Result<DialogReply, String> {
        let Some(Dialog::Form(form)) = self.pending.get(id) else {
            return Err("This request cannot be declined as a form.".into());
        };

        Ok(DialogReply {
            response: form.decline(),
            edit_prompt: false,
            retracted_messages: Vec::new(),
        })
    }

    pub(super) fn retire(&mut self, id: &str) -> Option<Vec<String>> {
        let dialog = self.pending.remove(id)?;

        self.retired.insert(id.to_owned());

        Some(dialog.retracted_messages().to_vec())
    }

    pub(super) fn finish(&mut self) -> Vec<String> {
        let ids: Vec<_> = self.pending.keys().cloned().collect();

        self.retired.extend(ids.iter().cloned());
        self.pending.clear();

        ids
    }
}
