use std::collections::HashMap;

pub(crate) fn event_dispatch_layers(
    compositor: &crate::compositor::Compositor,
    hit_layers: &[String],
    event_type: &str,
) -> Vec<String> {
    let mut layers = Vec::new();
    for id in hit_layers {
        let Some(handler) = compositor
            .scene()
            .get(id)
            .and_then(|layer| layer.event_handlers.get(event_type))
            .filter(|handler| handler.enabled)
        else {
            continue;
        };
        layers.push(id.clone());
        if !handler.penetration {
            break;
        }
    }
    // Penetration determines the chain top-down; handlers run bottom-up.
    layers.reverse();
    layers
}

pub(crate) fn layer_filter_params<'a>(
    handler: &'a crate::compositor::scene::LayerEventHandler,
    id: &str,
    event_type: &str,
) -> std::borrow::Cow<'a, HashMap<String, String>> {
    use std::borrow::Cow;

    if !handler.filter_params.is_empty() {
        return Cow::Borrowed(&handler.filter_params);
    }
    let mut params = handler.params.clone();
    params.insert("id".into(), id.into());
    params.insert("type".into(), event_type.into());
    for (key, value) in [
        ("file", handler.file.as_deref()),
        ("label", handler.label.as_deref()),
        ("handler", handler.handler.as_deref()),
        ("call", handler.call.then_some("1")),
        ("penetration", handler.penetration.then_some("1")),
    ] {
        if let Some(value) = value {
            params.insert(key.into(), value.into());
        }
    }
    Cow::Owned(params)
}

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
