use super::{CoreRuntime, PendingDialog};
use asb_interpreter::Value;
use serde_json::json;

impl CoreRuntime {
    pub(super) fn request_dialog(
        &mut self,
        title: &str,
        message: &str,
        varname: Option<&str>,
        textfield: Option<&str>,
        textfield_size: Option<usize>,
    ) {
        let initial_text = textfield
            .and_then(|name| self.interpreter.get_variable(name))
            .map(|value| value.as_string())
            .unwrap_or_default();

        self.pending_dialog = Some(PendingDialog {
            varname: varname.map(String::from),
            textfield: textfield.map(String::from),
            textfield_size,
        });

        if crate::ffi::ui_command_callback_registered() {
            crate::ffi::emit_ui_command(
                "dialog_show",
                json!({
                    "title": title,
                    "message": message,
                    "hasCancel": varname.is_some(),
                    "textfield": textfield.is_some(),
                    "textfieldSize": textfield_size,
                    "initialText": initial_text,
                }),
            );
        } else {
            crate::core_warn!("[dialog] 宿主未注册 UI 回调，dialog 将保持等待");
        }
    }

    pub fn submit_dialog_response(&mut self, accepted: bool, text: Option<&str>) -> bool {
        let Some(dialog) = self.pending_dialog.take() else {
            crate::core_warn!("[dialog] 收到响应时没有待处理 dialog");
            return false;
        };

        if let Some(varname) = dialog.varname {
            self.interpreter
                .set_variable(&varname, Value::Int(i64::from(accepted)));
        }
        if accepted && let Some(textfield) = dialog.textfield {
            let mut value = text.unwrap_or_default().to_string();
            if let Some(limit) = dialog.textfield_size {
                value = value.chars().take(limit).collect();
            }
            self.interpreter
                .set_variable(&textfield, Value::String(value));
        }

        crate::core_debug!("[dialog] response accepted={accepted}");
        self.advance_wait_line();
        true
    }
}
