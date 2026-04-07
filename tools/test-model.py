#!/usr/bin/env python3
"""Quick sanity check — load the fine-tuned LoRA and generate a response.

Loads Qwen2.5-Coder-1.5B base + our LoRA adapters, then asks it to
write some bare-metal Rust code to verify the fine-tune took effect.
"""

import sys
import io
from pathlib import Path

# Force UTF-8 stdout for Windows console
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8", errors="replace")

import torch
from transformers import AutoModelForCausalLM, AutoTokenizer, BitsAndBytesConfig
from peft import PeftModel

REPO_ROOT = Path(__file__).parent.parent
BASE_MODEL = "Qwen/Qwen2.5-Coder-1.5B"
LORA_DIR = REPO_ROOT / "models" / "claudio-coder-1.5b-lora"

SYSTEM_PROMPT = """You are a Rust systems programmer working on ClaudioOS, a bare-metal operating system that runs AI coding agents directly on x86_64 hardware.

Key constraints:
- All code is #![no_std] with extern crate alloc
- No Linux kernel, no POSIX, no JavaScript runtime
- Uses spin::Mutex for synchronization (no std Mutex)
- Uses smoltcp for networking, embedded-tls for TLS 1.3
- Cranelift for JIT compilation
- Raw HTTP/1.1 over TLS byte streams (no reqwest/hyper)
- Single address space, async executor, interrupt-driven

Write clean, well-commented Rust code that compiles for x86_64-unknown-none."""

TEST_PROMPTS = [
    "Describe the ClaudioOS project architecture in 2 sentences.",
    "In `crates/api-client/src/lib.rs`, implement a function `should_retry` that decides whether to retry an HTTP request based on status code.",
    "Write a no_std function that dequantizes Q4_0 GGUF tensor data to f32.",
    "What is the purpose of the `crates/rustc-lite/src/linker.rs` module?",
]


def main():
    print("=" * 60)
    print("ClaudioOS Model Sanity Check")
    print(f"Base: {BASE_MODEL}")
    print(f"LoRA: {LORA_DIR}")
    print("=" * 60)

    if not LORA_DIR.exists():
        print(f"ERROR: LoRA adapters not found at {LORA_DIR}")
        sys.exit(1)

    bnb_config = BitsAndBytesConfig(
        load_in_4bit=True,
        bnb_4bit_quant_type="nf4",
        bnb_4bit_compute_dtype=torch.float16,
        bnb_4bit_use_double_quant=True,
    )

    print("\nLoading base model in 4-bit...")
    model = AutoModelForCausalLM.from_pretrained(
        BASE_MODEL,
        quantization_config=bnb_config,
        device_map="auto",
        trust_remote_code=True,
        torch_dtype=torch.float16,
    )

    print("Loading LoRA adapters...")
    model = PeftModel.from_pretrained(model, str(LORA_DIR))
    # Put model in inference mode (disables dropout, batchnorm updates)
    model.train(False)

    print("Loading tokenizer...")
    tokenizer = AutoTokenizer.from_pretrained(BASE_MODEL, trust_remote_code=True)
    if tokenizer.pad_token is None:
        tokenizer.pad_token = tokenizer.eos_token

    print("\nReady. Running test prompts...\n")

    for i, prompt in enumerate(TEST_PROMPTS, 1):
        print(f"{'=' * 60}")
        print(f"TEST {i}/{len(TEST_PROMPTS)}")
        print(f"{'=' * 60}")
        print(f"Prompt: {prompt}\n")

        messages = [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": prompt},
        ]
        text = tokenizer.apply_chat_template(
            messages, tokenize=False, add_generation_prompt=True
        )

        inputs = tokenizer(text, return_tensors="pt").to(model.device)

        with torch.no_grad():
            outputs = model.generate(
                **inputs,
                max_new_tokens=400,
                temperature=0.3,
                do_sample=True,
                top_p=0.9,
                pad_token_id=tokenizer.pad_token_id,
                eos_token_id=tokenizer.eos_token_id,
            )

        response = tokenizer.decode(
            outputs[0][inputs["input_ids"].shape[1]:], skip_special_tokens=True
        )
        print(f"Response:\n{response}\n")


if __name__ == "__main__":
    main()
