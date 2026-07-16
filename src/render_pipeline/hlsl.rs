//! Translator for the small Direct3D9 HLSL effect subset used by Artemis.
//!
//! Artemis shader files contain a fixed pass/vertex wrapper and a compact
//! `ps()` pixel function. The renderer supplies its own vertex stage and turns
//! the pixel function plus global scalar declarations into GLSL.

const SAMPLERS: [(&str, &str); 4] = [
    ("samplerBack", "u_texture_back"),
    ("samplerFore", "u_texture_fore"),
    ("samplerMask", "u_texture_mask"),
    ("samplerUser", "u_texture_user"),
];

pub fn translate_effect(source: &[u8]) -> Result<String, String> {
    let decoded = String::from_utf8_lossy(source).replace('\r', "");
    let source = strip_comments(&decoded);
    let ps_start = source
        .find("void ps")
        .ok_or_else(|| "HLSL effect has no ps() function".to_string())?;
    let body_open = source[ps_start..]
        .find('{')
        .map(|offset| ps_start + offset)
        .ok_or_else(|| "HLSL ps() has no body".to_string())?;
    let body_close = matching_brace(&source, body_open)
        .ok_or_else(|| "HLSL ps() body is not balanced".to_string())?;

    let globals_end = source.find("void vs").unwrap_or(ps_start).min(ps_start);
    let globals = translate_globals(&source[..globals_end]);
    let body = translate_tokens(&source[body_open + 1..body_close]);

    Ok(format!(
        r#"
in vec2 v_uv;
out vec4 frag_color;

uniform sampler2D u_texture_back;
uniform sampler2D u_texture_fore;
uniform sampler2D u_texture_mask;
uniform sampler2D u_texture_user;
{globals}

void main() {{
    vec2 texCoord0 = v_uv;
    vec2 texCoord1 = v_uv;
    vec4 result = vec4(0.0);
{body}
    frag_color = result;
}}
"#
    ))
}

fn translate_globals(source: &str) -> String {
    let mut out = String::new();
    for statement in source.split(';') {
        let line = statement.trim();
        if line.is_empty()
            || line.starts_with("texture ")
            || line.starts_with("sampler ")
            || line.contains("sampler_state")
        {
            continue;
        }

        if line.starts_with("const float") {
            out.push_str(&translate_tokens(line));
            out.push_str(";\n");
        } else if line.starts_with("float") {
            out.push_str("uniform ");
            out.push_str(&translate_tokens(line));
            out.push_str(";\n");
        }
    }
    out
}

fn translate_tokens(source: &str) -> String {
    let mut out = replace_identifier(source, "float2", "vec2");
    out = replace_identifier(&out, "float3", "vec3");
    out = replace_identifier(&out, "float4", "vec4");
    out = replace_identifier(&out, "tex2D", "texture");
    for (hlsl, glsl) in SAMPLERS {
        out = replace_identifier(&out, hlsl, glsl);
    }
    convert_loop_index_multiplication(&out)
}

fn convert_loop_index_multiplication(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'*' {
            let mut cursor = index + 1;
            while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }
            let negative = bytes.get(cursor) == Some(&b'-');
            if negative {
                cursor += 1;
            }
            if bytes.get(cursor) == Some(&b'i')
                && bytes.get(cursor + 1).is_none_or(|byte| !is_ident(*byte))
            {
                out.push('*');
                if negative {
                    out.push_str(" -float(i)");
                } else {
                    out.push_str(" float(i)");
                }
                index = cursor + 1;
                continue;
            }
        }
        let ch = source[index..].chars().next().expect("valid char boundary");
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

fn replace_identifier(source: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if source[index..].starts_with(from) {
            let before = index.checked_sub(1).and_then(|i| bytes.get(i)).copied();
            let after = bytes.get(index + from.len()).copied();
            if before.is_none_or(|byte| !is_ident(byte)) && after.is_none_or(|byte| !is_ident(byte))
            {
                out.push_str(to);
                index += from.len();
                continue;
            }
        }
        let ch = source[index..].chars().next().expect("valid char boundary");
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn matching_brace(source: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, ch) in source[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '/' && chars.peek() == Some(&'/') {
            chars.next();
            for next in chars.by_ref() {
                if next == '\n' {
                    out.push('\n');
                    break;
                }
            }
        } else if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut previous = '\0';
            for next in chars.by_ref() {
                if previous == '*' && next == '/' {
                    break;
                }
                previous = next;
            }
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEPIA: &str = r#"
texture textureFore;
sampler samplerFore = sampler_state { texture = <textureFore>; };
float alpha;
float red;
const float3 graydata = float3(0.3, 0.6, 0.1);
void vs(float4 position : POSITION) { }
void ps(float2 texCoord0 : TEXCOORD0, float2 texCoord1 : TEXCOORD1,
        out float4 result : COLOR0)
{
    float4 fore = tex2D(samplerFore, texCoord1);
    float gray = dot(fore.rgb, graydata);
    fore.rgb = float3(gray * red, gray, gray);
    fore.a *= alpha;
    result = fore;
}
technique technique0 { }
"#;

    #[test]
    fn translates_artemis_effect_pixel_function() {
        let glsl = translate_effect(SEPIA.as_bytes()).unwrap();
        assert!(glsl.contains("uniform float alpha;"));
        assert!(glsl.contains("uniform float red;"));
        assert!(glsl.contains("const vec3 graydata = vec3"));
        assert!(glsl.contains("vec4 fore = texture(u_texture_fore, texCoord1);"));
        assert!(glsl.contains("frag_color = result;"));
        assert!(!glsl.contains("sampler_state"));
        assert!(!glsl.contains("float4"));
        assert!(!glsl.contains("tex2D"));
    }

    #[test]
    fn rejects_effect_without_pixel_function() {
        let error = translate_effect(b"void vs() {}").unwrap_err();
        assert!(error.contains("no ps()"));
    }
}
