use std::collections::HashMap;

pub(crate) fn enqueue_handler_tags(
    interpreter: &asb_interpreter::Interpreter,
    handler_tag: Option<&str>,
    file: Option<&str>,
    label: Option<&str>,
    call: bool,
    params: &HashMap<String, String>,
    runtime_params: &[(&str, &str)],
) -> bool {
    let tags = handler_tags(handler_tag, file, label, call, params, runtime_params);
    if tags.is_empty() {
        return false;
    }
    interpreter
        .engine_context()
        .lock()
        .unwrap()
        .tag_queue
        .extend(tags);
    true
}

pub(crate) fn handler_tags(
    handler_tag: Option<&str>,
    file: Option<&str>,
    label: Option<&str>,
    call: bool,
    params: &HashMap<String, String>,
    runtime_params: &[(&str, &str)],
) -> Vec<(String, HashMap<String, String>)> {
    // A target is optional only when *both* fields carry the sentinel.  Keep
    // a non-sentinel counterpart intact: `[call file="0" label="foo"]` is a
    // legitimate same-script call, while `[call file="foo" label="0"]`
    // remains an explicit (and therefore diagnosable) script target.
    let file = file.filter(|value| !value.is_empty());
    let label = label.filter(|value| !value.is_empty());
    let target_unset = file.map(|value| value == "0").unwrap_or(true)
        && label.map(|value| value == "0").unwrap_or(true);
    let (file, label) = if target_unset {
        (None, None)
    } else {
        (file, label)
    };
    let mut tags = Vec::new();
    if let Some(tag) = handler_tag {
        let mut p = params.clone();
        for (k, v) in runtime_params {
            p.insert(k.to_string(), v.to_string());
        }
        tags.push((tag.to_string(), p));
    }
    if file.is_some() || label.is_some() {
        let mut p = HashMap::new();
        if let Some(f) = file {
            p.insert("file".to_string(), f.to_string());
        }
        if let Some(l) = label {
            p.insert("label".to_string(), l.to_string());
        }
        tags.push((if call { "call" } else { "jump" }.to_string(), p));
    }
    tags
}
