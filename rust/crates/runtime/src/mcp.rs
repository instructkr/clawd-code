use crate::config::{McpServerConfig, ScopedMcpServerConfig};

const CLAUDEAI_SERVER_PREFIX: &str = "claude.ai ";
const CCR_PROXY_PATH_MARKERS: [&str; 2] = ["/v2/session_ingress/shttp/mcp/", "/v2/ccr-sessions/"];

#[must_use]
pub fn normalize_name_for_mcp(name: &str) -> String {
    let mut normalized = name
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' => ch,
            _ => '_',
        })
        .collect::<String>();

    if name.starts_with(CLAUDEAI_SERVER_PREFIX) {
        normalized = collapse_underscores(&normalized)
            .trim_matches('_')
            .to_string();
    }

    normalized
}

#[must_use]
pub fn mcp_tool_prefix(server_name: &str) -> String {
    format!("mcp__{}__", normalize_name_for_mcp(server_name))
}

#[must_use]
pub fn mcp_tool_name(server_name: &str, tool_name: &str) -> String {
    format!(
        "{}{}",
        mcp_tool_prefix(server_name),
        normalize_name_for_mcp(tool_name)
    )
}

#[must_use]
pub fn unwrap_ccr_proxy_url(url: &str) -> String {
    if !CCR_PROXY_PATH_MARKERS
        .iter()
        .any(|marker| url.contains(marker))
    {
        return url.to_string();
    }

    let Some(query_start) = url.find('?') else {
        return url.to_string();
    };
    let query = &url[query_start + 1..];
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        if matches!(parts.next(), Some("mcp_url")) {
            if let Some(value) = parts.next() {
                return percent_decode(value);
            }
        }
    }

    url.to_string()
}

#[must_use]
pub fn mcp_server_signature(config: &McpServerConfig) -> Option<String> {
    match config {
        McpServerConfig::Stdio(config) => {
            let mut command = vec![config.command.clone()];
            command.extend(config.args.clone());
            Some(format!("stdio:{}", render_command_signature(&command)))
        }
        McpServerConfig::Sse(config) | McpServerConfig::Http(config) => {
            Some(format!("url:{}", unwrap_ccr_proxy_url(&config.url)))
        }
        McpServerConfig::Ws(config) => Some(format!("url:{}", unwrap_ccr_proxy_url(&config.url))),
        McpServerConfig::ManagedProxy(config) => {
            Some(format!("url:{}", unwrap_ccr_proxy_url(&config.url)))
        }
        McpServerConfig::Sdk(_) => None,
    }
}

#[must_use]
pub fn scoped_mcp_config_hash(config: &ScopedMcpServerConfig) -> String {
    let rendered = match &config.config {
        McpServerConfig::Stdio(stdio) => format!(
            "stdio|{}|{}|{}|{}",
            stdio.command,
            render_command_signature(&stdio.args),
            render_env_signature(&stdio.env),
            stdio
                .tool_call_timeout_ms
                .map_or_else(String::new, |timeout_ms| timeout_ms.to_string())
        ),
        McpServerConfig::Sse(remote) => format!(
            "sse|{}|{}|{}|{}",
            remote.url,
            render_env_signature(&remote.headers),
            remote.headers_helper.as_deref().unwrap_or(""),
            render_oauth_signature(remote.oauth.as_ref())
        ),
        McpServerConfig::Http(remote) => format!(
            "http|{}|{}|{}|{}",
            remote.url,
            render_env_signature(&remote.headers),
            remote.headers_helper.as_deref().unwrap_or(""),
            render_oauth_signature(remote.oauth.as_ref())
        ),
        McpServerConfig::Ws(ws) => format!(
            "ws|{}|{}|{}",
            ws.url,
            render_env_signature(&ws.headers),
            ws.headers_helper.as_deref().unwrap_or("")
        ),
        McpServerConfig::Sdk(sdk) => format!("sdk|{}", sdk.name),
        McpServerConfig::ManagedProxy(proxy) => {
            format!("claudeai-proxy|{}|{}", proxy.url, proxy.id)
        }
    };
    stable_hex_hash(&format!("required:{}|{rendered}", config.required))
}

fn render_command_signature(command: &[String]) -> String {
    let escaped = command
        .iter()
        .map(|part| part.replace('\\', "\\\\").replace('|', "\\|"))
        .collect::<Vec<_>>();
    format!("[{}]", escaped.join("|"))
}

fn render_env_signature(map: &std::collections::BTreeMap<String, String>) -> String {
    map.iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(";")
}

fn render_oauth_signature(oauth: Option<&crate::config::McpOAuthConfig>) -> String {
    oauth.map_or_else(String::new, |oauth| {
        format!(
            "{}|{}|{}|{}",
            oauth.client_id.as_deref().unwrap_or(""),
            oauth
                .callback_port
                .map_or_else(String::new, |port| port.to_string()),
            oauth.auth_server_metadata_url.as_deref().unwrap_or(""),
            oauth.xaa.map_or_else(String::new, |flag| flag.to_string())
        )
    })
}

fn stable_hex_hash(value: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn collapse_underscores(value: &str) -> String {
    let mut collapsed = String::with_capacity(value.len());
    let mut last_was_underscore = false;
    for ch in value.chars() {
        if ch == '_' {
            if !last_was_underscore {
                collapsed.push(ch);
            }
            last_was_underscore = true;
        } else {
            collapsed.push(ch);
            last_was_underscore = false;
        }
    }
    collapsed
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = &value[index + 1..index + 3];
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    decoded.push(byte);
                    index += 3;
                    continue;
                }
                decoded.push(bytes[index]);
                index += 1;
            }
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::config::{
        ConfigSource, McpRemoteServerConfig, McpServerConfig, McpStdioServerConfig,
        McpWebSocketServerConfig, ScopedMcpServerConfig,
    };

    use super::{
        mcp_server_signature, mcp_tool_name, normalize_name_for_mcp, scoped_mcp_config_hash,
        unwrap_ccr_proxy_url,
    };

    #[test]
    fn normalizes_server_names_for_mcp_tooling() {
        assert_eq!(normalize_name_for_mcp("github.com"), "github_com");
        assert_eq!(normalize_name_for_mcp("tool name!"), "tool_name_");
        assert_eq!(
            normalize_name_for_mcp("claude.ai Example   Server!!"),
            "claude_ai_Example_Server"
        );
        assert_eq!(
            mcp_tool_name("claude.ai Example Server", "weather tool"),
            "mcp__claude_ai_Example_Server__weather_tool"
        );
    }

    #[test]
    fn unwraps_ccr_proxy_urls_for_signature_matching() {
        let wrapped = "https://api.anthropic.com/v2/session_ingress/shttp/mcp/123?mcp_url=https%3A%2F%2Fvendor.example%2Fmcp&other=1";
        assert_eq!(unwrap_ccr_proxy_url(wrapped), "https://vendor.example/mcp");
        assert_eq!(
            unwrap_ccr_proxy_url("https://vendor.example/mcp"),
            "https://vendor.example/mcp"
        );
    }

    #[test]
    fn computes_signatures_for_stdio_and_remote_servers() {
        let stdio = McpServerConfig::Stdio(McpStdioServerConfig {
            command: "uvx".to_string(),
            args: vec!["mcp-server".to_string()],
            env: BTreeMap::from([("TOKEN".to_string(), "secret".to_string())]),
            tool_call_timeout_ms: None,
        });
        assert_eq!(
            mcp_server_signature(&stdio),
            Some("stdio:[uvx|mcp-server]".to_string())
        );

        let remote = McpServerConfig::Ws(McpWebSocketServerConfig {
            url: "https://api.anthropic.com/v2/ccr-sessions/1?mcp_url=wss%3A%2F%2Fvendor.example%2Fmcp".to_string(),
            headers: BTreeMap::new(),
            headers_helper: None,
        });
        assert_eq!(
            mcp_server_signature(&remote),
            Some("url:wss://vendor.example/mcp".to_string())
        );
    }

    #[test]
    fn scoped_hash_ignores_scope_but_tracks_config_content() {
        let base_config = McpServerConfig::Http(McpRemoteServerConfig {
            url: "https://vendor.example/mcp".to_string(),
            headers: BTreeMap::from([("Authorization".to_string(), "Bearer token".to_string())]),
            headers_helper: Some("helper.sh".to_string()),
            oauth: None,
        });
        let user = ScopedMcpServerConfig {
            required: false,
            scope: ConfigSource::User,
            config: base_config.clone(),
        };
        let local = ScopedMcpServerConfig {
            required: false,
            scope: ConfigSource::Local,
            config: base_config,
        };
        assert_eq!(
            scoped_mcp_config_hash(&user),
            scoped_mcp_config_hash(&local)
        );

        let changed = ScopedMcpServerConfig {
            required: false,
            scope: ConfigSource::Local,
            config: McpServerConfig::Http(McpRemoteServerConfig {
                url: "https://vendor.example/v2/mcp".to_string(),
                headers: BTreeMap::new(),
                headers_helper: None,
                oauth: None,
            }),
        };
        assert_ne!(
            scoped_mcp_config_hash(&user),
            scoped_mcp_config_hash(&changed)
        );
    }
}
