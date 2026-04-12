use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "claudio-mux", version, about = "Terminal multiplexer for Windows")]
pub struct Cli {
    #[arg(short, long)]
    pub layout: Option<String>,

    #[arg(short, long, default_value = "main")]
    pub session: String,
}
