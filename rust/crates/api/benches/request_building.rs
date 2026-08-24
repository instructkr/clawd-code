// Benchmarks for API request building performance
// Benchmarks are exempt from strict linting as they are test/performance code
#![allow(
    clippy::cognitive_complexity,
    clippy::doc_markdown,
    clippy::explicit_iter_loop,
    clippy::format_in_format_args,
    clippy::missing_docs_in_private_items,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value,
    clippy::clone_on_copy,
    clippy::too_many_lines,
    clippy::uninlined_format_args
)]

use api::{
    build_chat_completion_request, flatten_tool_result_content, is_reasoning_model,
    translate_message, InputContentBlock, InputMessage, MessageRequest, OpenAiCompatConfig,
    ToolResultContentBlock,
};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use serde_json::json;

/// Create a sample message request with various content types
fn create_sample_request(message_count: usize) -> MessageRequest {
    let mut messages = Vec::with_capacity(message_count);

    for i in 0..message_count {
        match i % 4 {
            0 => messages.push(InputMessage::user_text(format!("Message {}", i))),
            1 => messages.push(InputMessage {
                role: "assistant".to_string(),
                content: vec![
                    InputContentBlock::Text {
                        text: format!("Assistant response {}", i),
                    },
                    InputContentBlock::ToolUse {
                        id: format!("call_{}", i),
                        name: "read_file".to_string(),
                        input: json!({"path": format!("/tmp/file{}", i)}),
                    },
                ],
            }),
            2 => messages.push(InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::ToolResult {
                    tool_use_id: format!("call_{}", i - 1),
                    content: vec![ToolResultContentBlock::Text {
                        text: format!("Tool result content {}", i),
                    }],
                    is_error: false,
                }],
            }),
            _ => messages.push(InputMessage {
                role: "assistant".to_string(),
                content: vec![InputContentBlock::ToolUse {
                    id: format!("call_{}", i),
                    name: "write_file".to_string(),
                    input: json!({"path": format!("/tmp/out{}", i), "content": "data"}),
                }],
            }),
        }
    }

    MessageRequest {
        model: "gpt-4o".to_string(),
        max_tokens: 1024,
        messages,
        stream: false,
        system: Some("You are a helpful assistant.".to_string()),
        temperature: Some(0.7),
        top_p: None,
        tools: None,
        tool_choice: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        reasoning_effort: None,
        extra_body: std::collections::BTreeMap::new(),
    }
}

/// Benchmark translate_message with various message types
fn bench_translate_message(c: &mut Criterion) {
    let mut group = c.benchmark_group("translate_message");

    // Text-only message
    let text_message = InputMessage::user_text("Simple text message".to_string());
    group.bench_with_input(
        BenchmarkId::new("text_only", "single"),
        &text_message,
        |b, msg| {
            b.iter(|| translate_message(black_box(msg), black_box("gpt-4o")));
        },
    );

    // Assistant message with tool calls
    let assistant_message = InputMessage {
        role: "assistant".to_string(),
        content: vec![
            InputContentBlock::Text {
                text: "I'll help you with that.".to_string(),
            },
            InputContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                input: json!({"path": "/tmp/test"}),
            },
            InputContentBlock::ToolUse {
                id: "call_2".to_string(),
                name: "write_file".to_string(),
                input: json!({"path": "/tmp/out", "content": "data"}),
            },
        ],
    };
    group.bench_with_input(
        BenchmarkId::new("assistant_with_tools", "2_tools"),
        &assistant_message,
        |b, msg| {
            b.iter(|| translate_message(black_box(msg), black_box("gpt-4o")));
        },
    );

    // Tool result message
    let tool_result_message = InputMessage {
        role: "user".to_string(),
        content: vec![InputContentBlock::ToolResult {
            tool_use_id: "call_1".to_string(),
            content: vec![ToolResultContentBlock::Text {
                text: "File contents here".to_string(),
            }],
            is_error: false,
        }],
    };
    group.bench_with_input(
        BenchmarkId::new("tool_result", "single"),
        &tool_result_message,
        |b, msg| {
            b.iter(|| translate_message(black_box(msg), black_box("gpt-4o")));
        },
    );

    // Tool result for kimi model (is_error excluded)
    group.bench_with_input(
        BenchmarkId::new("tool_result_kimi", "kimi-k2.5"),
        &tool_result_message,
        |b, msg| {
            b.iter(|| translate_message(black_box(msg), black_box("kimi-k2.5")));
        },
    );

    // Large content message
    let large_content = "x".repeat(10000);
    let large_message = InputMessage::user_text(large_content);
    group.bench_with_input(
        BenchmarkId::new("large_text", "10kb"),
        &large_message,
        |b, msg| {
            b.iter(|| translate_message(black_box(msg), black_box("gpt-4o")));
        },
    );

    group.finish();
}

/// Benchmark build_chat_completion_request with various message counts
fn bench_build_request(c: &mut Criterion) {
    let mut group = c.benchmark_group("build_chat_completion_request");
    let config = OpenAiCompatConfig::openai();

    for message_count in [10, 50, 100].iter() {
        let request = create_sample_request(*message_count);
        group.bench_with_input(
            BenchmarkId::new("message_count", message_count),
            &request,
            |b, req| {
                b.iter(|| build_chat_completion_request(black_box(req), config.clone()));
            },
        );
    }

    // Benchmark with reasoning model (tuning params stripped)
    let mut reasoning_request = create_sample_request(50);
    reasoning_request.model = "o1-mini".to_string();
    group.bench_with_input(
        BenchmarkId::new("reasoning_model", "o1-mini"),
        &reasoning_request,
        |b, req| {
            b.iter(|| build_chat_completion_request(black_box(req), config.clone()));
        },
    );

    // Benchmark with gpt-5 (max_completion_tokens)
    let mut gpt5_request = create_sample_request(50);
    gpt5_request.model = "gpt-5".to_string();
    group.bench_with_input(
        BenchmarkId::new("gpt5", "gpt-5"),
        &gpt5_request,
        |b, req| {
            b.iter(|| build_chat_completion_request(black_box(req), config.clone()));
        },
    );

    group.finish();
}

/// Benchmark flatten_tool_result_content
fn bench_flatten_tool_result(c: &mut Criterion) {
    let mut group = c.benchmark_group("flatten_tool_result_content");

    // Single text block
    let single_text = vec![ToolResultContentBlock::Text {
        text: "Simple result".to_string(),
    }];
    group.bench_with_input(
        BenchmarkId::new("single_text", "1_block"),
        &single_text,
        |b, content| {
            b.iter(|| flatten_tool_result_content(black_box(content)));
        },
    );

    // Multiple text blocks
    let multi_text: Vec<ToolResultContentBlock> = (0..10)
        .map(|i| ToolResultContentBlock::Text {
            text: format!("Line {}: some content here\n", i),
        })
        .collect();
    group.bench_with_input(
        BenchmarkId::new("multi_text", "10_blocks"),
        &multi_text,
        |b, content| {
            b.iter(|| flatten_tool_result_content(black_box(content)));
        },
    );

    // JSON content blocks
    let json_content: Vec<ToolResultContentBlock> = (0..5)
        .map(|i| ToolResultContentBlock::Json {
            value: json!({"index": i, "data": "test content", "nested": {"key": "value"}}),
        })
        .collect();
    group.bench_with_input(
        BenchmarkId::new("json_content", "5_blocks"),
        &json_content,
        |b, content| {
            b.iter(|| flatten_tool_result_content(black_box(content)));
        },
    );

    // Mixed content
    let mixed_content = vec![
        ToolResultContentBlock::Text {
            text: "Here's the result:".to_string(),
        },
        ToolResultContentBlock::Json {
            value: json!({"status": "success", "count": 42}),
        },
        ToolResultContentBlock::Text {
            text: "Processing complete.".to_string(),
        },
    ];
    group.bench_with_input(
        BenchmarkId::new("mixed_content", "text+json"),
        &mixed_content,
        |b, content| {
            b.iter(|| flatten_tool_result_content(black_box(content)));
        },
    );

    // Large content - simulating typical tool output
    let large_content: Vec<ToolResultContentBlock> = (0..50)
        .map(|i| {
            if i % 3 == 0 {
                ToolResultContentBlock::Json {
                    value: json!({"line": i, "content": "x".repeat(100)}),
                }
            } else {
                ToolResultContentBlock::Text {
                    text: format!("Line {}: {}", i, "some output content here"),
                }
            }
        })
        .collect();
    group.bench_with_input(
        BenchmarkId::new("large_content", "50_blocks"),
        &large_content,
        |b, content| {
            b.iter(|| flatten_tool_result_content(black_box(content)));
        },
    );

    group.finish();
}

/// Benchmark is_reasoning_model detection
fn bench_is_reasoning_model(c: &mut Criterion) {
    let mut group = c.benchmark_group("is_reasoning_model");

    let models = vec![
        ("gpt-4o", false),
        ("o1-mini", true),
        ("o3", true),
        ("grok-3", false),
        ("grok-3-mini", true),
        ("qwen/qwen-qwq-32b", true),
        ("qwen/qwen-plus", false),
    ];

    for (model, expected) in models {
        group.bench_with_input(
            BenchmarkId::new(model, if expected { "reasoning" } else { "normal" }),
            model,
            |b, m| {
                b.iter(|| is_reasoning_model(black_box(m)));
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_translate_message,
    bench_build_request,
    bench_flatten_tool_result,
    bench_is_reasoning_model
);
criterion_main!(benches);
