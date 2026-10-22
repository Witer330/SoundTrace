use crate::db::Db;
use crate::error::{err, AppResult};
use rusqlite::params;
use serde_json::json;
use std::io::Read;
use tauri::Emitter;

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SummarizeDone {
    pub recording_id: i64,
    pub ok: bool,
    pub error: String,
}

/// llm.thinking 设置值 → 请求里的思考模式开关（DeepSeek/GLM 的 `thinking.type`）。
/// 只有显式 "on" 才开，缺省关：纪要这种带硬格式的提炼任务，关思考更快更省、引用更忠实；
/// 思考型模型会把 max_tokens 预算整个耗在思维链上，正文返回空串。
pub fn thinking_param(setting: &str) -> &'static str {
    if setting.trim().eq_ignore_ascii_case("on") {
        "enabled"
    } else {
        "disabled"
    }
}

/// OpenAI 兼容 Chat Completions 调用（阻塞）
fn chat(
    base_url: &str,
    api_key: &str,
    model: &str,
    messages: &[(String, String)],
    max_tokens: u32,
    thinking: &str,
) -> AppResult<String> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let msgs: Vec<_> = messages
        .iter()
        .map(|(role, content)| json!({"role": role, "content": content}))
        .collect();
    let body = json!({
        "model": model,
        "messages": msgs,
        "temperature": 0.3,
        "max_tokens": max_tokens,
        "thinking": { "type": thinking },
    });

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| err(format!("创建 HTTP 客户端失败: {e}")))?;
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .map_err(|e| err(format!("请求失败: {e}")))?;

    let status = resp.status();
    let mut raw = String::new();
    resp.take(4 * 1024 * 1024).read_to_string(&mut raw)?;
    if !status.is_success() {
        return Err(err(format!("API 返回 {status}: {}", raw.chars().take(400).collect::<String>())));
    }
    parse_chat_response(&raw)
}

/// 从响应 JSON 取正文。截断（finish_reason=length）与空正文一律报错，绝不静默返回空串：
/// 思考型模型在预算耗尽时会返回 content=""，曾被当成成功写库，表现为“提示生成成功但纪要是空的”。
fn parse_chat_response(raw: &str) -> AppResult<String> {
    let parsed: serde_json::Value = serde_json::from_str(raw)?;
    if parsed.pointer("/choices/0/finish_reason").and_then(|v| v.as_str()) == Some("length") {
        return Err(err(
            "输出达到 max_tokens 上限被截断：若开启了深度思考，请调大输出上限或关闭深度思考后重试",
        ));
    }
    match parsed.pointer("/choices/0/message/content").and_then(|v| v.as_str()) {
        Some(c) if !c.trim().is_empty() => Ok(c.to_string()),
        Some(_) => Err(err("模型未返回正文：多为思考耗尽了 max_tokens 预算，请调大输出上限或关闭深度思考")),
        None => Err(err(format!(
            "API 响应格式异常: {}",
            raw.chars().take(300).collect::<String>()
        ))),
    }
}

/// 设置测试：发一条极短消息验证连通性（走与生成相同的思考开关，测的才是真实路径）。
/// max_tokens 给到 1024：开思考时极短消息也会先烧几十个思考 token，20 必被截断。
pub fn test_llm(base_url: &str, api_key: &str, model: &str, thinking: &str) -> AppResult<String> {
    let reply = chat(
        base_url,
        api_key,
        model,
        &[("user".into(), "回复“连接成功”四个字".into())],
        1024,
        thinking,
    )?;
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::{extract_tags, parse_chat_response, parse_headline, thinking_param};

    #[test]
    fn 首行引用块拆出类型与主题() {
        let md = "> 工作会议 · 讨论Q3采购报价\n\n## ✅ 关键结论\n- [00:00:12] 定了";
        assert_eq!(
            parse_headline(md),
            Some(("工作会议".into(), "讨论Q3采购报价".into()))
        );
    }

    #[test]
    fn 宽容分隔符与加粗包裹() {
        assert_eq!(
            parse_headline("> **课程学习** ・ 第一章导学"),
            Some(("课程学习".into(), "第一章导学".into()))
        );
        assert_eq!(
            parse_headline("> *访谈沟通* • 离职面谈"),
            Some(("访谈沟通".into(), "离职面谈".into()))
        );
    }

    #[test]
    fn 缺分隔符时整行当主题() {
        assert_eq!(parse_headline("> 关于采购流程的讨论"), Some(("".into(), "关于采购流程的讨论".into())));
    }

    #[test]
    fn 旧格式或空首行不误伤() {
        // 旧格式纪要没有首行引用块：解析为 None，回填时保持原标题
        assert_eq!(parse_headline("## ✅ 关键结论\n- 定了"), None);
        assert_eq!(parse_headline(""), None);
        assert_eq!(parse_headline(">   \n正文"), None);
        assert_eq!(parse_headline("> 工作会议 · "), None);
    }

    #[test]
    fn 思考开关只有on才算开() {
        assert_eq!(thinking_param("on"), "enabled");
        assert_eq!(thinking_param(" ON "), "enabled");
        assert_eq!(thinking_param(""), "disabled");
        assert_eq!(thinking_param("off"), "disabled");
    }

    #[test]
    fn 正常响应提取正文() {
        let raw = r#"{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"连接成功"}}]}"#;
        assert_eq!(parse_chat_response(raw).unwrap(), "连接成功");
    }

    #[test]
    fn 截断与空正文都报错而非静默空串() {
        let truncated =
            r#"{"choices":[{"finish_reason":"length","message":{"content":"","reasoning_content":"…"}}]}"#;
        assert!(parse_chat_response(truncated).is_err());
        let empty = r#"{"choices":[{"finish_reason":"stop","message":{"content":""}}]}"#;
        assert!(parse_chat_response(empty).is_err());
        let no_choices = r#"{"error":{"message":"Model Not Exist"}}"#;
        assert!(parse_chat_response(no_choices).is_err());
    }

    #[test]
    fn 提取反引号包裹的标签() {
        let md = "## 🏷️ 核心知识标签\n`#音频转码` `#FFmpeg` `#采样率`";
        assert_eq!(extract_tags(md), vec!["音频转码", "FFmpeg", "采样率"]);
    }

    #[test]
    fn 排除标题行识别紧跟标点的标签() {
        let md = "## ⚡ 核心结论\n- **[00:12:34] 转码**：定了。`#转码`";
        assert_eq!(extract_tags(md), vec!["转码"]);
    }

    #[test]
    fn 去重丢弃超长词并排除URL片段() {
        let md = "`#检索` `#检索` `#这条标签长得过分了必须截掉` `#ok,` 见 [文档](https://a.b/c#frag)";
        assert_eq!(extract_tags(md), vec!["检索", "ok"]);
    }
}

const FINAL_PROMPT: &str = r#"你是一名录音复盘助手。下面是一份带 [HH:MM:SS] 时间戳的录音逐字稿。先判断录音类型（工作会议 / 课程学习 / 访谈沟通），然后直接输出，不要任何客套话。

硬性规则：
1. 第一行是引用块，格式：> 类型 · 主题短语（≤20 字的名词短语，可直接用作录音标题；不用完整句，不带句号）。
2. 之后按固定分区输出，分区标题必须原样保留：## ✅ 关键结论、## 🎬 行动项、## ❓ 悬而未决、## 📖 名词速查、## 🏷️ 标签。
3. 每条一句话（≤40 字），按重要性排序，宁缺毋滥——只收听者复盘时真正需要的，全文要能一眼扫完。
4. 时间戳 [HH:MM:SS] 只能取自逐字稿中真实出现的，不得推算或编造。
5. 默认不引用原话；仅当原话本身就是口径（承诺、数字、验收标准）时可引用，且截短到一句以内。
6. 被明确否决的方案在「关键结论」末尾写一条：否决：方案——一句话理由。
7. 行动项格式：谁 → 做什么（期限）。没有期限写"期限未定"。课程录音写作业/练习，访谈写双方答应跟进的事。
8. 悬而未决收录：提出但没结论的、被搁置或话题被带走的、双方理解没对齐的；写清卡在哪。
9. 名词速查收录逐字稿里的术语、缩写、项目黑话：用大白话并结合本录音语境解释，一两句即可；逐字稿信息不够就写"录音里未展开"，不得编造；疑似同音误转的词按原词书写，可注明"疑为××"。
10. 某分区确无内容写"无"，不要硬凑。
11. 标签 3~5 个中文短词。

输出格式：
> 类型 · 一句话概括

## ✅ 关键结论
- [HH:MM:SS] 结论一句话

## 🎬 行动项
- [HH:MM:SS] 谁 → 做什么（期限）

## ❓ 悬而未决
- [HH:MM:SS] 什么事没定，卡在哪

## 📖 名词速查
- 术语：大白话解释

## 🏷️ 标签
`#标签1` `#标签2` `#标签3`"#;

const PARTIAL_PROMPT: &str = r#"你是一名录音复盘助手。以下是长录音的其中一个片段（带 [HH:MM:SS] 时间戳）。直接输出四部分候选，不要客套：

1. 候选结论：本片段敲定或否决的内容，每条：- [HH:MM:SS] 一句话（≤40字）
2. 候选行动项：每条 - [HH:MM:SS] 谁 → 做什么（期限，没有写"期限未定"）
3. 未决候选：每条 - [HH:MM:SS] 什么事没定，卡在哪
4. 术语记录：每条 - 术语 | 语境线索（是否反复出现、有没有人没听懂）

只依据片段内容，时间戳必须来自片段原文，不要编造。"#;

const MERGE_PROMPT: &str = r#"你是一名录音复盘助手。下面是同一份录音按时间顺序各片段的候选分析。先判断录音类型（工作会议 / 课程学习 / 访谈沟通），去重合并后直接输出最终结果，不要客套：

规则：
1. 第一行引用块：> 类型 · 主题短语（≤20 字的名词短语，可直接用作录音标题；不用完整句，不带句号）。
2. 分区标题必须原样保留：## ✅ 关键结论、## 🎬 行动项、## ❓ 悬而未决、## 📖 名词速查、## 🏷️ 标签。
3. 结论只保留真正敲定的，跨片段有修订以最后一版为准；被否决的写"否决：方案——理由"；每条一句话（≤40 字），按重要性排序。
4. 行动项格式：谁 → 做什么（期限）。
5. 术语跨片段反复出现的是重点，合并去重后用大白话结合语境解释；信息不足写"录音里未展开"，不得编造。
6. 时间戳必须来自片段分析中出现过的原时间戳；某分区确无内容写"无"。
7. 标签 3~5 个中文短词。"#;

/// 从生成的 Markdown 里提取 `#标签`（排除 `##` 标题行与 URL #fragment），供回写标签表
fn extract_tags(md: &str) -> Vec<String> {
    fn tag_start(prev: Option<char>) -> bool {
        match prev {
            None => true,
            Some(c) => c.is_whitespace() || c == '`' || c == '*' || c == '(',
        }
    }

    let mut out: Vec<String> = Vec::new();
    let chars: Vec<char> = md.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // 只有行首/空白/Markdown 包裹符后的 '#' 才是标签；URL fragment 前是路径字符，自然排除
        if chars[i] != '#' || (i > 0 && !tag_start(Some(chars[i - 1]))) {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut end = start;
        while end < chars.len()
            && !chars[end].is_whitespace()
            && chars[end] != '#'
            && chars[end] != '`'
        {
            end += 1;
        }
        let name: String = chars[start..end].iter().collect();
        let name = name.trim_end_matches(|c: char| c.is_ascii_punctuation());
        // 上限用于滤掉误入的长 token，超长直接丢弃而非截断，避免产生半个词
        if !name.is_empty() && name.chars().count() <= 12 && !out.iter().any(|x| x == name) {
            out.push(name.to_string());
        }
        i = end;
    }
    out
}

/// 解析纪要首行「> 类型 · 主题」，返回 (类型, 主题)；主题始终非空。
/// None = 首个非空行不是引用块（旧格式纪要）或没有主题。
/// 分隔符宽容 `·`/`・`/`•`；缺分隔符时整行当主题、类型为空串。
/// 供标题自动回填（db 迁移与 summarize 收尾）与前端类型徽章共用同一规则。
pub fn parse_headline(md: &str) -> Option<(String, String)> {
    let clean = |s: &str| {
        s.trim_matches(|c: char| c.is_whitespace() || c == '*' || c == '_')
            .trim()
            .to_string()
    };
    let line = md.lines().map(str::trim).find(|l| !l.is_empty())?;
    let body = line.strip_prefix('>')?.trim();
    if body.is_empty() {
        return None;
    }
    for (i, c) in body.char_indices() {
        if c == '·' || c == '・' || c == '•' {
            let kind = clean(&body[..i]);
            let topic = clean(&body[i + c.len_utf8()..]);
            return if topic.is_empty() { None } else { Some((kind, topic)) };
        }
    }
    Some((String::new(), clean(body)))
}

fn fmt_ts(ms: i64) -> String {
    let s = ms / 1000;
    format!("[{:02}:{:02}:{:02}]", s / 3600, (s / 60) % 60, s % 60)
}

/// 生成会议纪要（后台线程），完成发 summarize://done 事件
pub fn summarize(db: std::sync::Arc<Db>, app: tauri::AppHandle, recording_id: i64) {
    std::thread::spawn(move || {
        let result = (|| -> AppResult<String> {
            let (base_url, api_key, model, thinking, title, recorded_at, participants, texts) = {
                let conn = db.conn.lock().unwrap();
                let get_setting = |key: &str| -> String {
                    conn.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| r.get(0))
                        .unwrap_or_default()
                };
                let config = (
                    get_setting("llm.baseUrl"),
                    get_setting("llm.apiKey"),
                    get_setting("llm.model"),
                    get_setting("llm.thinking"),
                );
                let rec: (String, Option<String>, String) = conn
                    .query_row(
                        "SELECT title, recorded_at, participants FROM recordings WHERE id=?1",
                        [recording_id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .map_err(|_| err("录音不存在"))?;
                let mut stmt = conn.prepare(
                    "SELECT start_ms, text FROM segments WHERE recording_id=?1 ORDER BY start_ms",
                )?;
                let texts: Vec<String> = stmt
                    .query_map([recording_id], |r| {
                        Ok(format!("{} {}", fmt_ts(r.get::<_, i64>(0)?), r.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                (
                    config.0,
                    config.1,
                    config.2,
                    thinking_param(&config.3),
                    rec.0,
                    rec.1,
                    rec.2,
                    texts,
                )
            };

            if base_url.is_empty() || api_key.is_empty() || model.is_empty() {
                return Err(err("请先在设置中配置 LLM（Base URL / API Key / 模型）"));
            }
            if texts.is_empty() {
                return Err(err("尚无转写内容，请先完成转写"));
            }

            // max_tokens 是防失控的保险丝，不是目标产量（计费按实际生成，给大不花钱）：
            // 关思考时正文实测约 2K token，16K 已有数倍余量；开思考时思维链本身就要数万
            let (cap_final, cap_partial) =
                if thinking == "enabled" { (65536, 32768) } else { (16384, 8192) };

            let header = format!(
                "会议主题：{title}\n日期：{}\n参会人：{}\n\n",
                recorded_at.as_deref().map(|s| &s[..s.len().min(10)]).unwrap_or("未知"),
                if participants.is_empty() { "未记录" } else { &participants },
            );

            let full = texts.join("\n");
            // 单次直读上限 6 万字符（约 3~4 小时录音）：术语「反复解释」这类跨距离信号，
            // 只有整篇直读才看得全，能不分片就不分片
            let summary = if full.chars().count() <= 60000 {
                chat(
                    &base_url,
                    &api_key,
                    &model,
                    &[
                        ("system".into(), FINAL_PROMPT.into()),
                        ("user".into(), format!("{header}会议转写稿：\n\n{full}")),
                    ],
                    cap_final,
                    thinking,
                )?
            } else {
                // 长转写：分段小结 → 合并
                let mut partials: Vec<String> = Vec::new();
                let mut buf = String::new();
                for line in &texts {
                    if buf.chars().count() + line.chars().count() > 25000 {
                        partials.push(
                            chat(
                                &base_url,
                                &api_key,
                                &model,
                                &[
                                    ("system".into(), PARTIAL_PROMPT.into()),
                                    ("user".into(), format!("{header}片段内容：\n\n{buf}")),
                                ],
                                cap_partial,
                                thinking,
                            )?,
                        );
                        buf.clear();
                    }
                    buf.push_str(line);
                    buf.push('\n');
                }
                if !buf.is_empty() {
                    partials.push(
                        chat(
                            &base_url,
                            &api_key,
                            &model,
                            &[
                                ("system".into(), PARTIAL_PROMPT.into()),
                                ("user".into(), format!("{header}片段内容：\n\n{buf}")),
                            ],
                            cap_partial,
                            thinking,
                        )?,
                    );
                }
                chat(
                    &base_url,
                    &api_key,
                    &model,
                    &[
                        ("system".into(), MERGE_PROMPT.into()),
                        ("user".into(), format!("{header}各片段要点：\n\n{}", partials.join("\n\n---\n\n"))),
                    ],
                    cap_final,
                    thinking,
                )?
            };

            // 保存
            let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "UPDATE recordings SET summary_md=?1, llm_model=?2, updated_at=?3 WHERE id=?4",
                params![summary, model, now, recording_id],
            )?;
            // 纪要首行的主题回填为列表标题：只碰 title_edited=0（未手工命名）的行，
            // 手工命名不被覆盖（章程：手工修改不可静默丢失）
            if let Some((_, topic)) = parse_headline(&summary) {
                conn.execute(
                    "UPDATE recordings SET title=?1 WHERE id=?2 AND title_edited=0 AND title<>?1",
                    params![topic, recording_id],
                )?;
            }
            // LLM 生成的知识标签合并进标签表（只增不改，不碰手工标签），让全局搜索能搜到
            for name in extract_tags(&summary) {
                conn.execute(
                    "INSERT INTO tags(name) VALUES(?1) ON CONFLICT(name) DO NOTHING",
                    [&name],
                )?;
                let tag_id: i64 = conn.query_row(
                    "SELECT id FROM tags WHERE name = ?1",
                    [&name],
                    |r| r.get(0),
                )?;
                conn.execute(
                    "INSERT OR IGNORE INTO recording_tags(recording_id, tag_id) VALUES(?1, ?2)",
                    params![recording_id, tag_id],
                )?;
            }
            Ok(summary)
        })();

        let _ = app.emit(
            "summarize://done",
            SummarizeDone {
                recording_id,
                ok: result.is_ok(),
                error: result.err().map(|e| e.msg).unwrap_or_default(),
            },
        );
    });
}
