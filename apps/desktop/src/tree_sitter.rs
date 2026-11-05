use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, process::Command};

#[derive(Default, Serialize, Deserialize)]
pub struct Settings {
    pub executable: String,
    pub config: String,
}

fn settings_path() -> Option<PathBuf> {
    Some(crate::project::config_dir()?.join("tree-sitter.json"))
}

pub fn load() -> Settings {
    settings_path()
        .and_then(|p| fs::read(p).ok())
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

pub fn save(settings: &Settings) -> Result<(), String> {
    let path = settings_path().ok_or("无法确定设置目录")?;
    let dir = path.parent().ok_or("无效的设置目录")?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut file, settings).map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.error.to_string())?;
    Ok(())
}

pub fn executable(settings: &Settings) -> PathBuf {
    if !settings.executable.trim().is_empty() {
        return PathBuf::from(settings.executable.trim());
    }
    if let Some(dir) = crate::project::config_dir() {
        let managed = dir.join("tree-sitter").join("bin").join(if cfg!(windows) {
            "tree-sitter.exe"
        } else {
            "tree-sitter"
        });
        if managed.is_file() {
            return managed;
        }
    }
    PathBuf::from("tree-sitter")
}

pub fn detect(settings: &Settings) -> Result<String, String> {
    let executable = executable(settings);
    let output = Command::new(&executable)
        .arg("--version")
        .output()
        .map_err(|e| format!("无法运行 {}：{e}", executable.display()))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(format!(
        "{} · {}",
        executable.display(),
        String::from_utf8_lossy(&output.stdout).trim()
    ))
}

pub fn languages(settings: &Settings) -> Result<String, String> {
    let mut command = Command::new(executable(settings));
    command.arg("dump-languages");
    if !settings.config.trim().is_empty() {
        command.arg("--config-path").arg(settings.config.trim());
    }
    let output = command
        .output()
        .map_err(|e| format!("无法启动 Tree-sitter：{e}"))?;
    if !output.status.success() {
        return Err(format!(
            "语言扫描失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let names: Vec<_> = text
        .lines()
        .filter_map(|line| line.strip_prefix("name: "))
        .collect();
    if names.is_empty() {
        Ok("没有发现语言解析器，请检查 Tree-sitter 配置文件中的 parser-directories。".into())
    } else {
        Ok(format!(
            "已发现 {} 个语言解析器：{}",
            names.len(),
            names.join(", ")
        ))
    }
}

pub fn install() -> Result<String, String> {
    let root = crate::project::config_dir()
        .ok_or("无法确定设置目录")?
        .join("tree-sitter");
    let output = Command::new("cargo")
        .args(["install", "tree-sitter-cli", "--locked", "--root"])
        .arg(&root)
        .output()
        .map_err(|e| format!("无法运行 Cargo：{e}"))?;
    if !output.status.success() {
        return Err(format!(
            "安装失败：{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(format!("安装完成：{}", root.display()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn detects_tool_and_passes_config_to_language_scan() {
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("tree-sitter");
        fs::write(&tool, "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'tree-sitter 1.2.3'; elif [ \"$1\" = dump-languages ] && [ \"$2\" = --config-path ] && [ \"$3\" = 'my config.json' ]; then echo 'name: c'; else exit 1; fi\n").unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        let settings = Settings {
            executable: tool.to_string_lossy().into_owned(),
            config: "my config.json".into(),
        };
        assert!(detect(&settings).unwrap().contains("tree-sitter 1.2.3"));
        assert_eq!(languages(&settings).unwrap(), "已发现 1 个语言解析器：c");
    }

    #[test]
    fn parses_nested_captures_entities_and_final_newline() {
        let html = "<pre><code><span class='keyword'>if</span> (a &lt; <span class='string'>\"猫 &amp; dog\"</span>)</code></pre>";
        let lines = parse_fragment(html, "if (a < \"猫 & dog\")\n").unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0][0].class, "keyword");
        assert!(lines[0]
            .iter()
            .any(|span| span.text == "\"猫 & dog\"" && span.class == "string"));
        assert!(parse_fragment(html, "different").is_err());
    }

    #[test]
    fn parses_cli_table_without_line_numbers_and_preserves_lines() {
        let html = "<!doctype HTML><table>\n<tr><td class=line-number>1</td><td class=line><span class='type builtin'>int</span> x;\n</td></tr>\n<tr><td class=line-number>2</td><td class=line>\n</td></tr>\n</table>";
        let lines = parse_fragment(html, "int x;\n\n").unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0][0].class, "type.builtin");
        assert_eq!(
            lines[0].iter().map(|s| s.text.as_str()).collect::<String>(),
            "int x;"
        );
        assert!(lines[1].is_empty());
        assert!(lines[2].is_empty());
        assert!(parse_fragment(html, "wrong source").is_err());
        assert!(parse_fragment("<table></table>", "int x;").is_err());
        assert!(parse_fragment("<table><td class=line>x", "x").is_err());
    }

    #[test]
    fn removes_only_synthetic_final_newline() {
        let html =
            "<table><tr><td class=line-number>1</td><td class=line>int x;\n</td></tr></table>";
        assert_eq!(parse_fragment(html, "int x;").unwrap().len(), 1);
        assert_eq!(parse_fragment(html, "int x;\n").unwrap().len(), 2);
        let empty = "<table><tr><td class=line>\n</td></tr></table>";
        assert_eq!(parse_fragment(empty, "").unwrap(), vec![vec![]]);
    }

    #[test]
    fn preserves_cli_diagnostics_on_highlight_failure() {
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("tree-sitter");
        fs::write(
            &tool,
            "#!/bin/sh\necho 'parser compilation failed' >&2\nexit 2\n",
        )
        .unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        let settings = Settings {
            executable: tool.to_string_lossy().into_owned(),
            config: String::new(),
        };
        let error = highlight(&settings, std::path::Path::new("test.c"), "int x;").unwrap_err();
        assert!(error.contains("parser compilation failed"));
        assert!(error.contains(tool.to_str().unwrap()));
        assert!(error.contains('2'));
    }

    // Run explicitly on machines with the CLI and a configured C grammar.
    #[test]
    #[ignore = "requires tree-sitter CLI and a configured C grammar"]
    fn highlights_with_installed_cli() {
        let settings = Settings::default();
        for text in [
            "",
            "int x;",
            "int x;\n",
            "int x;\n\n",
            "/* 猫 & dog */\nint x;\n",
        ] {
            let lines = highlight(&settings, std::path::Path::new("test.c"), text).unwrap();
            let reconstructed = lines
                .iter()
                .map(|line| line.iter().map(|s| s.text.as_str()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(reconstructed, text);
            if text.contains("int") {
                assert!(lines
                    .iter()
                    .flatten()
                    .any(|span| span.text == "int" && span.class.starts_with("type")));
            }
        }
    }

    #[test]
    fn highlights_the_decoded_document_using_configured_cli() {
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("tree-sitter");
        fs::write(&tool, r#"#!/bin/sh
[ "$1" = highlight ] && [ "$2" = --html ] && [ "$3" = --css-classes ] || exit 1
[ "$4" = --config-path ] && [ "$5" = 'my config.json' ] || exit 3
for last do :; done
[ "$(cat "$last")" = 'int x;' ] || exit 2
printf "<table><tr><td class=line-number>1</td><td class=line><span class='type'>int</span> x;\n</td></tr></table>"
"#).unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        let settings = Settings {
            executable: tool.to_string_lossy().into_owned(),
            config: "my config.json".into(),
        };
        let lines = highlight(&settings, std::path::Path::new("test.c"), "int x;\n").unwrap();
        assert_eq!(lines[0][0].class, "type");
        assert_eq!(lines[0][0].text, "int");
        assert_eq!(lines.len(), 2);
    }
}

/// A single capture from the CLI's HTML fragment, before conversion to UI data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub class: String,
}

fn decode_html(text: &str) -> Result<String, String> {
    let mut result = String::new();
    let mut rest = text;
    while let Some(pos) = rest.find('&') {
        result.push_str(&rest[..pos]);
        rest = &rest[pos + 1..];
        let end = rest.find(';').ok_or("无效的 HTML 实体")?;
        let entity = &rest[..end];
        match entity {
            "amp" => result.push('&'),
            "lt" => result.push('<'),
            "gt" => result.push('>'),
            "quot" => result.push('"'),
            "apos" | "#39" => result.push('\''),
            _ if entity.starts_with("#x") => result.push(
                char::from_u32(
                    u32::from_str_radix(&entity[2..], 16).map_err(|_| "无效的 HTML 实体")?,
                )
                .ok_or("无效的 HTML 字符")?,
            ),
            _ if entity.starts_with('#') => result.push(
                char::from_u32(entity[1..].parse().map_err(|_| "无效的 HTML 实体")?)
                    .ok_or("无效的 HTML 字符")?,
            ),
            _ => return Err("未知的 HTML 实体".into()),
        }
        rest = &rest[end + 1..];
    }
    result.push_str(rest);
    Ok(result)
}

fn extract_table_code(table: &str) -> Result<String, String> {
    let mut code = String::new();
    let mut rest = table;
    while let Some(start) = rest.find("<td") {
        rest = &rest[start + 3..];
        let end = rest.find('>').ok_or("无效的高亮单元格")?;
        let attributes = rest[..end].trim();
        let content = &rest[end + 1..];
        let (cell, tail) = content.split_once("</td>").ok_or("高亮单元格不完整")?;
        if matches!(attributes, "class=line" | "class='line'" | "class=\"line\"") {
            code.push_str(cell);
        }
        rest = tail;
    }
    if code.is_empty() {
        return Err("缺少高亮内容，请检查语言解析器配置".into());
    }
    Ok(code)
}

fn parse_fragment(html: &str, original: &str) -> Result<Vec<Vec<Span>>, String> {
    // CLI releases emit a full HTML table. Keep accepting fragments as well.
    let table_code;
    let html = if html.contains("<table>") {
        let table = html
            .split_once("<table>")
            .unwrap()
            .1
            .split_once("</table>")
            .ok_or("高亮表格不完整")?
            .0;
        table_code = format!("<pre><code>{}</code></pre>", extract_table_code(table)?);
        table_code.as_str()
    } else {
        html
    };
    let code = html
        .split_once("<pre><code>")
        .ok_or("缺少高亮内容")?
        .1
        .split_once("</code></pre>")
        .ok_or("高亮内容不完整")?
        .0;
    let mut stack: Vec<String> = Vec::new();
    let mut spans = Vec::new();
    let mut rest = code;
    while !rest.is_empty() {
        if rest.starts_with("<span class=") {
            let end = rest.find('>').ok_or("无效的高亮标签")?;
            let tag = &rest[..=end];
            let class = tag
                .strip_prefix("<span class='")
                .and_then(|s| s.strip_suffix("'>"))
                .or_else(|| {
                    tag.strip_prefix("<span class=\"")
                        .and_then(|s| s.strip_suffix("\">"))
                })
                .ok_or("无效的高亮类别")?;
            stack.push(class.split_whitespace().collect::<Vec<_>>().join("."));
            rest = &rest[end + 1..];
        } else if let Some(tail) = rest.strip_prefix("</span>") {
            stack.pop().ok_or("多余的高亮结束标签")?;
            rest = tail;
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            if end == 0 {
                return Err("未知的高亮标签".into());
            }
            spans.push(Span {
                text: decode_html(&rest[..end])?,
                class: stack.last().cloned().unwrap_or_default(),
            });
            rest = &rest[end..];
        }
    }
    if !stack.is_empty() {
        return Err("高亮标签未闭合".into());
    }
    let mut reconstructed: String = spans.iter().map(|s| s.text.as_str()).collect();
    // Table output always terminates its last row, even without a source newline.
    if reconstructed.strip_suffix('\n') == Some(original) {
        if let Some(last) = spans.last_mut() {
            last.text.pop();
        }
        reconstructed.pop();
    }
    // Fragment output can omit one final newline.
    if reconstructed != original && original.strip_suffix('\n') != Some(reconstructed.as_str()) {
        return Err("高亮内容与源码不一致".into());
    }
    if reconstructed.len() < original.len() {
        spans.push(Span {
            text: "\n".into(),
            class: String::new(),
        });
    }
    let mut lines = vec![Vec::new()];
    for span in spans {
        let mut pieces = span.text.split('\n').peekable();
        while let Some(piece) = pieces.next() {
            if !piece.is_empty() {
                lines.last_mut().unwrap().push(Span {
                    text: piece.to_owned(),
                    class: span.class.clone(),
                });
            }
            if pieces.peek().is_some() {
                lines.push(Vec::new());
            }
        }
    }
    Ok(lines)
}

pub fn highlight(
    settings: &Settings,
    path: &std::path::Path,
    text: &str,
) -> Result<Vec<Vec<Span>>, String> {
    use std::io::Write;
    if text.len() > 1024 * 1024 {
        return Err("文件超过高亮大小限制（1 MiB）".into());
    }
    if text.bytes().filter(|byte| *byte == b'\n').count() > 50_000 {
        return Err("文件超过高亮行数限制（50000 行）".into());
    }
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .ok_or("文件没有扩展名")?;
    if extension.len() > 32
        || !extension
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err("不支持的文件扩展名".into());
    }
    let mut file = tempfile::Builder::new()
        .suffix(&format!(".{extension}"))
        .tempfile()
        .map_err(|e| e.to_string())?;
    file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    let executable = executable(settings);
    let mut command = Command::new(&executable);
    command.args(["highlight", "--html", "--css-classes"]);
    if !settings.config.trim().is_empty() {
        command.arg("--config-path").arg(settings.config.trim());
    }
    let output = command
        .arg(file.path())
        .output()
        .map_err(|e| format!("无法启动 {}：{e}", executable.display()))?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(format!(
            "Tree-sitter 高亮失败（{}，{}）：{}",
            executable.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if output.stdout.len() > 16 * 1024 * 1024 {
        return Err("高亮结果过大".into());
    }
    let html = std::str::from_utf8(&output.stdout).map_err(|e| e.to_string())?;
    parse_fragment(html, text).map_err(|error| {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.trim().is_empty() {
            error
        } else {
            format!("{error}：{}", stderr.trim())
        }
    })
}
