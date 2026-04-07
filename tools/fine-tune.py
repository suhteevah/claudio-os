#!/usr/bin/env python3
"""QLoRA fine-tune Qwen2.5-Coder-7B on ClaudioOS training data.

Runs on a single RTX 3070 Ti (8GB VRAM). Uses 4-bit quantization for
the base model and LoRA adapters for parameter-efficient fine-tuning.

Prerequisites:
    pip install torch transformers peft trl bitsandbytes accelerate datasets

Usage:
    # Generate training data first:
    python tools/generate-training-data.py

    # Then fine-tune:
    python tools/fine-tune.py

    # Export to GGUF after training:
    python tools/export-gguf.py
"""

import json
import os
import sys
from pathlib import Path

# Check dependencies before importing
try:
    import torch
    from transformers import (
        AutoModelForCausalLM,
        AutoTokenizer,
        BitsAndBytesConfig,
        TrainingArguments,
    )
    from peft import LoraConfig, get_peft_model, prepare_model_for_kbit_training
    from trl import SFTTrainer, SFTConfig
    from datasets import Dataset
except ImportError as e:
    print(f"Missing dependency: {e}")
    print("Install with: pip install torch transformers peft trl bitsandbytes accelerate datasets")
    sys.exit(1)

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

# Base model — best coding model that fits in 8GB VRAM with QLoRA
BASE_MODEL = "Qwen/Qwen2.5-Coder-7B"

# Paths
REPO_ROOT = Path(__file__).parent.parent
TRAINING_DATA = REPO_ROOT / "tools" / "training-data.jsonl"
OUTPUT_DIR = REPO_ROOT / "models" / "claudio-coder-7b-lora"
MERGED_DIR = REPO_ROOT / "models" / "claudio-coder-7b-merged"

# Training hyperparameters (tuned for 3070 Ti 8GB)
LORA_R = 16                    # LoRA rank — 16 is good quality/memory tradeoff
LORA_ALPHA = 32                # LoRA scaling factor (usually 2x rank)
LORA_DROPOUT = 0.05            # Small dropout to prevent overfitting
MAX_SEQ_LEN = 2048             # Context window for training (saves VRAM vs 4096)
BATCH_SIZE = 1                 # Must be 1 for 8GB VRAM
GRADIENT_ACCUM = 8             # Effective batch size = 8
LEARNING_RATE = 2e-4           # Standard for QLoRA
NUM_EPOCHS = 4                 # Bumped to 4 for better recall on 7B model
WARMUP_RATIO = 0.03            # Warm up for 3% of training
SAVE_STEPS = 200               # Checkpoint every 200 steps

# LoRA target modules for Qwen2.5
TARGET_MODULES = [
    "q_proj", "k_proj", "v_proj", "o_proj",  # Attention
    "gate_proj", "up_proj", "down_proj",       # FFN (SwiGLU)
]


# ---------------------------------------------------------------------------
# Data loading
# ---------------------------------------------------------------------------

def load_training_data() -> Dataset:
    """Load the JSONL training data into a HuggingFace Dataset."""
    if not TRAINING_DATA.exists():
        print(f"Training data not found at {TRAINING_DATA}")
        print("Run: python tools/generate-training-data.py")
        sys.exit(1)

    examples = []
    with open(TRAINING_DATA, "r", encoding="utf-8") as f:
        for line in f:
            ex = json.loads(line)
            # Convert ShareGPT format to a single text string
            text = format_conversation(ex["conversations"])
            examples.append({"text": text})

    print(f"Loaded {len(examples)} training examples")
    return Dataset.from_list(examples)


def format_conversation(messages: list[dict]) -> str:
    """Format a ShareGPT conversation into a single training string.

    Uses Qwen's chat template format:
    <|im_start|>system\n...<|im_end|>
    <|im_start|>user\n...<|im_end|>
    <|im_start|>assistant\n...<|im_end|>
    """
    parts = []
    for msg in messages:
        role = msg["from"]
        value = msg["value"]
        # Map ShareGPT roles to Qwen chat roles
        if role == "system":
            parts.append(f"<|im_start|>system\n{value}<|im_end|>")
        elif role == "human":
            parts.append(f"<|im_start|>user\n{value}<|im_end|>")
        elif role == "gpt":
            parts.append(f"<|im_start|>assistant\n{value}<|im_end|>")
    return "\n".join(parts)


# ---------------------------------------------------------------------------
# Model setup
# ---------------------------------------------------------------------------

def setup_model():
    """Load the base model in 4-bit and apply LoRA."""
    print(f"Loading {BASE_MODEL} in 4-bit quantization...")

    # 4-bit quantization config for QLoRA
    bnb_config = BitsAndBytesConfig(
        load_in_4bit=True,
        bnb_4bit_quant_type="nf4",          # Normal Float 4 — best for QLoRA
        bnb_4bit_compute_dtype=torch.float32, # fp32 compute avoids bf16 grad scaler crash
        bnb_4bit_use_double_quant=True,       # Double quantization saves ~0.4GB
    )

    # Pin every weight to GPU 0. device_map="auto" sees ~6.7 GB free and
    # decides 7B-NF4 won't fit, then tries CPU offload, which BnB rejects.
    # Forcing {"": 0} skips the planner — if it OOMs we surface a real
    # OOM instead of a confusing "modules dispatched on CPU" error.
    model = AutoModelForCausalLM.from_pretrained(
        BASE_MODEL,
        quantization_config=bnb_config,
        device_map={"": 0},
        trust_remote_code=True,
        torch_dtype=torch.float32,
    )

    # Load tokenizer
    tokenizer = AutoTokenizer.from_pretrained(
        BASE_MODEL,
        trust_remote_code=True,
    )
    if tokenizer.pad_token is None:
        tokenizer.pad_token = tokenizer.eos_token

    # Prepare for k-bit training
    model = prepare_model_for_kbit_training(model)

    # Apply LoRA
    lora_config = LoraConfig(
        r=LORA_R,
        lora_alpha=LORA_ALPHA,
        target_modules=TARGET_MODULES,
        lora_dropout=LORA_DROPOUT,
        bias="none",
        task_type="CAUSAL_LM",
    )
    model = get_peft_model(model, lora_config)

    # Cast any bf16 parameters to fp16 to avoid gradient scaler crash on 3070 Ti
    for name, param in model.named_parameters():
        if param.dtype == torch.bfloat16:
            param.data = param.data.to(torch.float16)

    # Print trainable parameters
    trainable, total = model.get_nb_trainable_parameters()
    print(f"Trainable: {trainable:,} / {total:,} parameters ({100*trainable/total:.2f}%)")

    return model, tokenizer


# ---------------------------------------------------------------------------
# Training
# ---------------------------------------------------------------------------

def train(model, tokenizer, dataset):
    """Run QLoRA fine-tuning."""
    os.makedirs(OUTPUT_DIR, exist_ok=True)

    training_args = SFTConfig(
        output_dir=str(OUTPUT_DIR),
        num_train_epochs=NUM_EPOCHS,
        per_device_train_batch_size=BATCH_SIZE,
        gradient_accumulation_steps=GRADIENT_ACCUM,
        learning_rate=LEARNING_RATE,
        warmup_ratio=WARMUP_RATIO,
        lr_scheduler_type="cosine",
        fp16=False,                         # Disabled — bitsandbytes bf16 conflicts with AMP scaler
        bf16=False,                         # 3070 Ti lacks full bf16 support
        logging_steps=10,
        save_steps=SAVE_STEPS,
        save_total_limit=3,                 # Keep last 3 checkpoints
        max_length=MAX_SEQ_LEN,
        gradient_checkpointing=True,        # Saves ~2GB VRAM at cost of speed
        optim="paged_adamw_8bit",           # 8-bit optimizer saves VRAM
        report_to="none",                   # No wandb/tensorboard
        dataset_text_field="text",
    )

    trainer = SFTTrainer(
        model=model,
        train_dataset=dataset,
        processing_class=tokenizer,
        args=training_args,
    )

    print(f"\nStarting training: {NUM_EPOCHS} epochs, {len(dataset)} examples")
    print(f"Effective batch size: {BATCH_SIZE * GRADIENT_ACCUM}")
    print(f"Output: {OUTPUT_DIR}\n")

    trainer.train()
    trainer.save_model(str(OUTPUT_DIR))
    tokenizer.save_pretrained(str(OUTPUT_DIR))

    print(f"\nTraining complete! LoRA adapters saved to: {OUTPUT_DIR}")


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    print("=" * 60)
    print("ClaudioOS Model Fine-Tuning")
    print(f"Base model: {BASE_MODEL}")
    print(f"Training data: {TRAINING_DATA}")
    print(f"Target GPU: RTX 3070 Ti (8GB VRAM)")
    print("=" * 60)

    # Check CUDA
    if not torch.cuda.is_available():
        print("ERROR: CUDA not available. Need an NVIDIA GPU.")
        sys.exit(1)

    # bitsandbytes 0.49+ uses bf16 internally for NF4 quantized params.
    # The 3070 Ti's AMP gradient scaler can't unscale bf16 grads.
    # Workaround: set compute dtype to fp32 and disable AMP entirely.
    # Slower but actually trains correctly.
    print("Note: Using fp32 compute (bf16 AMP not supported on 3070 Ti)")

    gpu_name = torch.cuda.get_device_name(0)
    gpu_mem = torch.cuda.get_device_properties(0).total_memory / (1024**3)
    print(f"GPU: {gpu_name} ({gpu_mem:.1f} GB)")

    dataset = load_training_data()
    model, tokenizer = setup_model()
    train(model, tokenizer, dataset)

    print(f"\nNext steps:")
    print(f"  1. Merge LoRA weights:  python tools/export-gguf.py")
    print(f"  2. Serve locally:       llama-server -m models/claudio-coder-7b.gguf --port 8080 -ngl 99")
    print(f"  3. Test:                curl localhost:8080/v1/chat/completions -d '{{\"messages\": [...]}}'")


if __name__ == "__main__":
    main()
