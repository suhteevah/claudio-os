#!/usr/bin/env python3
"""Merge LoRA adapters and export to GGUF format for deployment.

Run after fine-tune.py completes. Produces an F16 GGUF (and optionally
Q4_0 quantized) file ready for llama.cpp inference or loading into
ClaudioOS's bare-metal LLM engine via init_local_model_from_bytes.

Prerequisites:
    pip install torch transformers peft
    git clone https://github.com/ggerganov/llama.cpp
    cd llama.cpp && make   # only needed for Q4_0 quantization

Usage:
    python tools/export-gguf.py                       # 7B (default)
    python tools/export-gguf.py --size 1.5b           # 1.5B
    python tools/export-gguf.py --size 1.5b --no-quantize
"""

import argparse
import os
import sys
import subprocess
from pathlib import Path

try:
    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer
    from peft import PeftModel
except ImportError as e:
    print(f"Missing dependency: {e}")
    sys.exit(1)

REPO_ROOT = Path(__file__).parent.parent

SIZES = {
    "1.5b": ("Qwen/Qwen2.5-Coder-1.5B", "claudio-coder-1.5b"),
    "7b":   ("Qwen/Qwen2.5-Coder-7B",   "claudio-coder-7b"),
}

# Filled in by main() once args are parsed.
BASE_MODEL = ""
LORA_DIR = Path()
MERGED_DIR = Path()
GGUF_F16 = Path()
GGUF_Q4 = Path()

# Path to llama.cpp tools (adjust if installed elsewhere)
LLAMA_CPP = Path(os.environ.get("LLAMA_CPP", str(Path.home() / "llama.cpp")))


def merge_lora():
    """Merge LoRA adapters back into the base model."""
    if not LORA_DIR.exists():
        print(f"LoRA adapters not found at {LORA_DIR}")
        print("Run fine-tune.py first.")
        sys.exit(1)

    print(f"Loading base model: {BASE_MODEL}")
    base_model = AutoModelForCausalLM.from_pretrained(
        BASE_MODEL,
        torch_dtype=torch.float16,
        device_map="cpu",  # merge on CPU to avoid VRAM pressure
        trust_remote_code=True,
    )

    print(f"Loading LoRA adapters from: {LORA_DIR}")
    model = PeftModel.from_pretrained(base_model, str(LORA_DIR))

    print("Merging LoRA weights into base model...")
    model = model.merge_and_unload()

    os.makedirs(MERGED_DIR, exist_ok=True)
    print(f"Saving merged model to: {MERGED_DIR}")
    model.save_pretrained(str(MERGED_DIR), safe_serialization=True)

    tokenizer = AutoTokenizer.from_pretrained(BASE_MODEL, trust_remote_code=True)
    tokenizer.save_pretrained(str(MERGED_DIR))

    print("Merge complete!")
    return MERGED_DIR


def convert_to_gguf(merged_dir: Path):
    """Convert HuggingFace model to GGUF format."""
    convert_script = LLAMA_CPP / "convert_hf_to_gguf.py"
    if not convert_script.exists():
        print(f"llama.cpp not found at {LLAMA_CPP}")
        print(f"Set LLAMA_CPP env var or clone to ~/llama.cpp:")
        print(f"  git clone https://github.com/ggerganov/llama.cpp")
        print(f"  cd llama.cpp && make")
        sys.exit(1)

    print(f"\nConverting to GGUF (F16)...")
    cmd = [
        sys.executable, str(convert_script),
        str(merged_dir),
        "--outfile", str(GGUF_F16),
        "--outtype", "f16",
    ]
    subprocess.run(cmd, check=True)
    print(f"F16 GGUF: {GGUF_F16} ({GGUF_F16.stat().st_size / (1024**3):.2f} GB)")


def quantize(gguf_f16: Path):
    """Quantize F16 GGUF to Q4_0 for fast inference."""
    quantize_bin = LLAMA_CPP / "build" / "bin" / "llama-quantize"
    if not quantize_bin.exists():
        # Try alternative path
        quantize_bin = LLAMA_CPP / "llama-quantize"
    if not quantize_bin.exists():
        print(f"llama-quantize not found. Build llama.cpp first:")
        print(f"  cd {LLAMA_CPP} && make")
        print(f"\nYou can quantize manually later:")
        print(f"  llama-quantize {GGUF_F16} {GGUF_Q4} q4_0")
        return

    print(f"\nQuantizing to Q4_0...")
    cmd = [str(quantize_bin), str(gguf_f16), str(GGUF_Q4), "q4_0"]
    subprocess.run(cmd, check=True)
    print(f"Q4_0 GGUF: {GGUF_Q4} ({GGUF_Q4.stat().st_size / (1024**3):.2f} GB)")


def main():
    global BASE_MODEL, LORA_DIR, MERGED_DIR, GGUF_F16, GGUF_Q4
    ap = argparse.ArgumentParser()
    ap.add_argument("--size", choices=SIZES.keys(), default="7b")
    ap.add_argument("--no-quantize", action="store_true",
                    help="skip Q4_0 quantization (useful when llama-quantize is not built)")
    args = ap.parse_args()

    BASE_MODEL, slug = SIZES[args.size]
    LORA_DIR   = REPO_ROOT / "models" / f"{slug}-lora"
    MERGED_DIR = REPO_ROOT / "models" / f"{slug}-merged"
    GGUF_F16   = REPO_ROOT / "models" / f"{slug}-f16.gguf"
    GGUF_Q4    = REPO_ROOT / "models" / f"{slug}-q4_0.gguf"

    print("=" * 60)
    print(f"ClaudioOS Model Export to GGUF — {args.size}")
    print(f"Base: {BASE_MODEL}")
    print(f"LoRA: {LORA_DIR}")
    print("=" * 60)

    merged = merge_lora()
    convert_to_gguf(merged)
    if not args.no_quantize:
        quantize(GGUF_F16)

    print(f"\n{'=' * 60}")
    print(f"Done! Your model is ready:")
    print(f"  Q4_0 GGUF: {GGUF_Q4}")
    print(f"\nTo serve locally:")
    print(f"  llama-server -m {GGUF_Q4} --host 0.0.0.0 --port 8080 -ngl 99")
    print(f"\nTo test:")
    print(f"  curl http://localhost:8080/v1/chat/completions \\")
    print(f"    -H 'Content-Type: application/json' \\")
    print(f"    -d '{{\"model\": \"claudio-coder\", \"messages\": [{{\"role\": \"user\", \"content\": \"Write a no_std GGUF parser\"}}]}}'")


if __name__ == "__main__":
    main()
