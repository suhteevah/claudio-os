# r/LocalLLaMA Post

**Subreddit**: r/LocalLLaMA
**Title**: Bare-metal OS purpose-built for running AI agents with GPU compute scaffolding

---

**TLDR**: ClaudioOS is a bare-metal Rust OS (no Linux) designed for running AI coding agents on dedicated hardware. Currently runs Claude via the Anthropic API, but the GPU compute driver (NVIDIA, nouveau-style MMIO) and tensor operation layer are scaffolded for future local inference. 222K lines of Rust, 38 crates, boots in under 2 seconds.

---

## What this is

ClaudioOS is a bare-metal operating system written entirely in Rust that boots on x86_64 UEFI hardware and runs AI coding agents. Today it runs Claude agents via the Anthropic API. The roadmap includes local LLM inference on NVIDIA GPUs without CUDA or Linux.

I know "without CUDA or Linux" sounds insane. Let me explain where things actually stand.

## Current state: API-based agents (working)

Right now, ClaudioOS boots, brings up networking (TCP/IP via smoltcp, TLS 1.3, DHCP, DNS), authenticates to api.anthropic.com, and runs multiple Claude agent sessions in parallel. Each agent has:

- Split-pane terminal with independent scroll
- Conversation state management
- Tool loop (editor, Python interpreter, Rust compiler, web browser, shell)
- Up to 20 rounds of tool use per turn

This works today in QEMU and on real hardware. The Anthropic Max subscription gives API access.

## GPU driver: scaffolding, not functional

The `gpu-compute-nostd` crate (3,392 lines) lays out the architecture for bare-metal NVIDIA GPU access:

```
tensor.rs     - TensorDescriptor, matmul, softmax, layernorm, GELU
compute.rs    - Compute class setup, shader load, grid dispatch
fifo.rs       - GPFIFO channels, push buffers, runlists, doorbells
falcon.rs     - Falcon microcontroller: PMU, SEC2, GSP-RM firmware
memory.rs     - VRAM allocation, GPU page tables, DMA mapping
mmio.rs       - NV_PMC, PFIFO, PFB, PGRAPH register definitions
pci_config.rs - PCI vendor 0x10DE detect, BAR mapping
driver.rs     - High-level GpuDevice init, query, compute API
```

This is modeled on the nouveau project's reverse-engineering work. The register definitions, Falcon firmware loading sequence, and GPFIFO channel setup are all based on envytools documentation.

**Honest status**: This is scaffolding. Real GPU initialization requires uploading signed firmware to Falcon microcontrollers, constructing GPU page tables, programming the FIFO engine, and speaking the compute class protocol. NVIDIA keeps most of this undocumented. nouveau has spent 15+ years reverse-engineering it. We are standing on their shoulders but have not completed the climb.

## The roadmap to local inference

Here is the path from where we are to running local models:

1. **GPU init** (hard): Get the Falcon firmware loaded, GPU page tables set up, and a compute channel running. This is the biggest engineering challenge.
2. **Tensor operations** (medium): Matrix multiply, softmax, layer norm, GELU -- all need to run as GPU compute dispatches. The API is designed but not connected to real hardware.
3. **Model loading** (medium): Read GGUF/safetensors from NVMe storage (NVMe driver exists, 2,563 lines), parse model architecture, allocate VRAM.
4. **Inference engine** (medium): KV cache management, token sampling, prompt processing. Pure Rust, no Python.
5. **Quantization** (easier): INT4/INT8 quantized inference to fit models in available VRAM.

## Why bare metal for inference?

If this ever works, the advantages over Linux + CUDA would be:

- **Zero overhead**: No kernel/user boundary, no CUDA runtime, no driver stack. GPU commands go straight from the application to MMIO registers.
- **Boot time**: Under 2 seconds to a running model vs. 30+ seconds for Linux + driver load + model init.
- **Memory**: No OS consuming VRAM. No CUDA runtime overhead. Every byte goes to your model.
- **Determinism**: No other processes, no swap, no OOM killer. Predictable inference latency.
- **Simplicity**: One binary, one purpose. Flash it to a USB, boot, infer.

## What exists today that is useful

Even without local inference, ClaudioOS has components that might interest this community:

- **Tensor operation API** (in `gpu-compute-nostd`): TensorDescriptor with shape/stride/dtype, operations for matmul, softmax, layernorm, GELU. Ready to be wired to a GPU backend or run on CPU.
- **NVMe driver** (2,563 lines): Fast storage access for loading large model files.
- **SMP support** (3,391 lines): Multi-core with per-CPU data and work-stealing scheduler. Useful for CPU-side preprocessing.
- **Python interpreter** (2,388 lines): For scripting model evaluation and benchmarks.

## Related: the full OS

Beyond the GPU/inference angle, ClaudioOS is a complete bare-metal OS:

- 4 filesystem drivers (ext4, btrfs, NTFS, FAT32) + VFS
- VirtIO-net + Intel NIC + WiFi drivers
- TLS 1.3 networking
- Post-quantum SSH daemon
- Linux syscall compatibility (150+ syscalls, ELF loader)
- Text editor, shell, JavaScript evaluator, Rust compiler

222K lines of Rust, 38 crates, 29 published as standalone `no_std` libraries.

## Links

- **GitHub**: https://github.com/suhteevah/claudio-os
- **Website**: https://claudioos.vercel.app
- **GPU crate source**: https://github.com/suhteevah/claudio-os/tree/main/crates/gpu
- **All 29 standalone crates**: https://github.com/suhteevah/claudio-os/blob/main/docs/OPEN-SOURCE-CRATES.md

I am not going to pretend local inference is right around the corner. The GPU driver is the hardest unsolved problem in this project. But the architecture is designed for it, and I think a bare-metal inference engine is an interesting enough goal to be worth pursuing. Would love to hear from anyone who has worked with nouveau or bare-metal GPU programming.
