use async_openai::{Client, config::OpenAIConfig};
use clap::Parser;
use serde::Serialize;
use serde_json::{Value, json};
use std::{env, fs, path::Path, process, process::Command};

#[derive(Parser)]
#[command(author, version, about)]
struct Args {
    #[arg(short = 'p', long)]
    prompt: String,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Value],
    tools: &'a [Tool],
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Tool {
    Function { function: FunctionDef },
}

#[derive(Serialize)]
struct FunctionDef {
    name: String,
    description: String,
    parameters: Value,
}

const MODEL: &str = "anthropic/claude-haiku-4.5";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let base_url = env::var("OPENROUTER_BASE_URL")
        .unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_string());

    let api_key = env::var("OPENROUTER_API_KEY").unwrap_or_else(|_| {
        eprintln!("OPENROUTER_API_KEY is not set");
        process::exit(1);
    });

    let config = OpenAIConfig::new()
        .with_api_base(base_url)
        .with_api_key(api_key);

    let client = Client::with_config(config);

    let tools = vec![
        Tool::Function {
            function: FunctionDef {
                name: "Read".to_string(),
                description: "Read and return the contents of a file".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "The path to the file to read"
                        }
                    },
                    "required": ["file_path"]
                }),
            },
        },
        Tool::Function {
            function: FunctionDef {
                name: "Write".to_string(),
                description: "Write content to a file. Creates the file if it does not exist, overwrites it if it does.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "The path of the file to write to"
                        },
                        "content": {
                            "type": "string",
                            "description": "The content to write to the file"
                        }
                    },
                    "required": ["file_path", "content"]
                }),
            },
        },
        Tool::Function {
            function: FunctionDef {
                name: "Bash".to_string(),
                description: "Execute a shell command and return its combined stdout and stderr.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to execute"
                        }
                    },
                    "required": ["command"]
                }),
            },
        },
    ];

    let mut messages: Vec<Value> = vec![json!({
        "role": "user",
        "content": args.prompt,
    })];

    loop {
        let request = ChatRequest {
            model: MODEL,
            messages: &messages,
            tools: &tools,
        };

        let response: Value = client.chat().create_byot(&request).await?;
        let message = response["choices"][0]["message"].clone();
        messages.push(message.clone());

        let tool_calls = message["tool_calls"].as_array();
        let has_tool_calls = tool_calls.is_some_and(|c| !c.is_empty());

        if !has_tool_calls {
            if let Some(content) = message["content"].as_str() {
                println!("{}", content);
            }
            return Ok(());
        }

        for tool_call in tool_calls.unwrap() {
            let id = tool_call["id"].as_str().unwrap_or("");
            let name = tool_call["function"]["name"].as_str().unwrap_or("");
            let arguments = tool_call["function"]["arguments"].as_str().unwrap_or("{}");
            let result = execute_tool(name, arguments);

            messages.push(json!({
                "role": "tool",
                "tool_call_id": id,
                "content": result,
            }));
        }
    }
}

fn execute_tool(name: &str, arguments: &str) -> String {
    let args: Value = match serde_json::from_str(arguments) {
        Ok(v) => v,
        Err(e) => return format!("Error: invalid arguments JSON: {}", e),
    };

    match name {
        "Read" => {
            let file_path = args["file_path"].as_str().unwrap_or("");
            match fs::read_to_string(file_path) {
                Ok(contents) => contents,
                Err(e) => format!("Error reading {}: {}", file_path, e),
            }
        }
        "Write" => {
            let file_path = args["file_path"].as_str().unwrap_or("");
            let content = args["content"].as_str().unwrap_or("");
            match write_file(file_path, content) {
                Ok(()) => format!("Successfully wrote to {}", file_path),
                Err(e) => format!("Error writing {}: {}", file_path, e),
            }
        }
        "Bash" => {
            let command = args["command"].as_str().unwrap_or("");
            run_bash(command)
        }
        other => format!("Error: unsupported tool '{}'", other),
    }
}

fn run_bash(command: &str) -> String {
    let output = match Command::new("sh").arg("-c").arg(command).output() {
        Ok(o) => o,
        Err(e) => return format!("Error executing command: {}", e),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let mut result = String::new();
    result.push_str(&stdout);
    if !stderr.is_empty() {
        if !result.is_empty() && !result.ends_with('\n') {
            result.push('\n');
        }
        result.push_str(&stderr);
    }
    if !output.status.success() {
        if !result.is_empty() && !result.ends_with('\n') {
            result.push('\n');
        }
        result.push_str(&format!(
            "Command exited with status: {}",
            output.status.code().map(|c| c.to_string()).unwrap_or_else(|| "unknown".to_string())
        ));
    }
    result
}

fn write_file(file_path: &str, content: &str) -> std::io::Result<()> {
    if let Some(parent) = Path::new(file_path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(file_path, content)
}
