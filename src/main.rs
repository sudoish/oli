use async_openai::{Client, config::OpenAIConfig};
use clap::Parser;
use serde::Serialize;
use serde_json::{Value, json};
use std::{env, fs, process};

#[derive(Parser)]
#[command(author, version, about)]
struct Args {
    #[arg(short = 'p', long)]
    prompt: String,
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    tools: Vec<Tool>,
}

#[derive(Serialize)]
struct Message {
    role: String,
    content: String,
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

    let request = ChatRequest {
        model: "anthropic/claude-haiku-4.5".to_string(),
        messages: vec![Message {
            role: "user".to_string(),
            content: args.prompt,
        }],
        tools: vec![Tool::Function {
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
        }],
    };

    let response: Value = client.chat().create_byot(&request).await?;

    let message = &response["choices"][0]["message"];

    if let Some(tool_call) = message["tool_calls"].as_array().and_then(|c| c.first()) {
        let name = tool_call["function"]["name"].as_str().unwrap_or("");
        let arguments = tool_call["function"]["arguments"].as_str().unwrap_or("{}");
        let args: Value = serde_json::from_str(arguments)?;

        match name {
            "Read" => {
                let file_path = args["file_path"].as_str().unwrap_or("");
                let contents = fs::read_to_string(file_path)?;
                print!("{}", contents);
            }
            other => {
                eprintln!("Unsupported tool call: {}", other);
                process::exit(1);
            }
        }
    } else if let Some(content) = message["content"].as_str() {
        println!("{}", content);
    }

    Ok(())
}
