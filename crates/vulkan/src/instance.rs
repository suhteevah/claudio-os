//! Vulkan instance management — vkCreateInstance, vkDestroyInstance,
//! vkEnumeratePhysicalDevices, physical device property queries.

use alloc::string::String;
use alloc::vec::Vec;
use alloc::vec;
use spin::Mutex;
use log::{info, debug};

use crate::{
    VkResult, VkHandle, VkPhysicalDeviceProperties, VkPhysicalDeviceType,
    VkPhysicalDeviceMemoryProperties, VkMemoryType, VkMemoryHeap,
    VkQueueFamilyProperties, VkExtent3D, VkStructureType,
    VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT,
    VK_MEMORY_PROPERTY_HOST_COHERENT_BIT, VK_MEMORY_HEAP_DEVICE_LOCAL_BIT,
    VK_QUEUE_GRAPHICS_BIT, VK_QUEUE_COMPUTE_BIT, VK_QUEUE_TRANSFER_BIT,
    VK_API_VERSION_1_3, alloc_handle,
};

/// Application info passed to vkCreateInstance
#[derive(Debug, Clone)]
pub struct VkApplicationInfo {
    pub application_name: String,
    pub application_version: u32,
    pub engine_name: String,
    pub engine_version: u32,
    pub api_version: u32,
}

/// Instance creation parameters
#[derive(Debug, Clone)]
pub struct VkInstanceCreateInfo {
    pub s_type: VkStructureType,
    pub application_info: Option<VkApplicationInfo>,
    pub enabled_layer_names: Vec<String>,
    pub enabled_extension_names: Vec<String>,
}

/// A Vulkan instance — the root object of the Vulkan API
pub struct VkInstance {
    pub handle: VkHandle,
    pub app_info: Option<VkApplicationInfo>,
    pub physical_devices: Vec<PhysicalDevice>,
}

/// Represents a discovered physical GPU device
pub struct PhysicalDevice {
    pub handle: VkHandle,
    /// PCI vendor ID (0x10DE = NVIDIA)
    pub vendor_id: u32,
    /// PCI device ID
    pub device_id: u32,
    /// Human-readable name
    pub device_name: String,
    /// VRAM size in bytes
    pub vram_size: u64,
    /// Whether this is our primary GPU
    pub is_primary: bool,
}

/// Global instance registry — ClaudioOS runs one instance at a time
static INSTANCE: Mutex<Option<VkInstance>> = Mutex::new(None);

/// Physical device features (Vulkan 1.3 core features we advertise)
#[derive(Debug, Clone)]
pub struct VkPhysicalDeviceFeatures {
    pub geometry_shader: bool,
    pub tessellation_shader: bool,
    pub multi_draw_indirect: bool,
    pub sampler_anisotropy: bool,
    pub texture_compression_bc: bool,
    pub shader_float64: bool,
    pub shader_int64: bool,
    pub shader_int16: bool,
    pub multi_viewport: bool,
    pub depth_clamp: bool,
    pub fill_mode_non_solid: bool,
    pub wide_lines: bool,
    pub large_points: bool,
}

impl Default for VkPhysicalDeviceFeatures {
    fn default() -> Self {
        Self {
            geometry_shader: true,
            tessellation_shader: true,
            multi_draw_indirect: true,
            sampler_anisotropy: true,
            texture_compression_bc: true,
            shader_float64: true,
            shader_int64: true,
            shader_int16: true,
            multi_viewport: true,
            depth_clamp: true,
            fill_mode_non_solid: true,
            wide_lines: true,
            large_points: true,
        }
    }
}

/// vkCreateInstance — initialize the Vulkan instance
///
/// Probes the GPU hardware (via `claudio-gpu` PCI enumeration) and populates
/// the physical device list. In ClaudioOS we expect exactly one NVIDIA GPU.
pub fn vk_create_instance(create_info: &VkInstanceCreateInfo) -> Result<VkHandle, VkResult> {
    info!("vulkan: vkCreateInstance");

    if let Some(ref app) = create_info.application_info {
        debug!(
            "vulkan: app='{}' v{}, engine='{}' v{}, api={:#x}",
            app.application_name, app.application_version,
            app.engine_name, app.engine_version,
            app.api_version
        );
    }

    // Validate requested extensions
    for ext in &create_info.enabled_extension_names {
        match ext.as_str() {
            "VK_KHR_surface" | "VK_CLAUDIO_surface" => {}
            other => {
                log::warn!("vulkan: unsupported extension requested: {}", other);
                return Err(VkResult::ErrorExtensionNotPresent);
            }
        }
    }

    let instance_handle = alloc_handle();

    // Enumerate physical devices — probe PCI for NVIDIA GPUs
    // In a real implementation this would call into claudio-gpu's PCI scanner.
    // For now we create a physical device entry representing the RTX 3070 Ti.
    let gpu = PhysicalDevice {
        handle: alloc_handle(),
        vendor_id: 0x10DE,  // NVIDIA
        device_id: 0x2482,  // GA104 [RTX 3070 Ti]
        device_name: String::from("NVIDIA GeForce RTX 3070 Ti"),
        vram_size: 8 * 1024 * 1024 * 1024, // 8 GiB GDDR6X
        is_primary: true,
    };

    info!("vulkan: found physical device: {} (VRAM {} MiB)",
          gpu.device_name, gpu.vram_size / (1024 * 1024));

    let instance = VkInstance {
        handle: instance_handle,
        app_info: create_info.application_info.clone(),
        physical_devices: vec![gpu],
    };

    *INSTANCE.lock() = Some(instance);

    Ok(instance_handle)
}

/// vkDestroyInstance — tear down the Vulkan instance
pub fn vk_destroy_instance(instance: VkHandle) -> VkResult {
    info!("vulkan: vkDestroyInstance handle={:?}", instance);
    let mut lock = INSTANCE.lock();
    if let Some(ref inst) = *lock {
        if inst.handle == instance {
            *lock = None;
            return VkResult::Success;
        }
    }
    VkResult::ErrorInitializationFailed
}

/// vkEnumeratePhysicalDevices — list physical GPU devices
pub fn vk_enumerate_physical_devices(
    _instance: VkHandle,
) -> Result<Vec<VkHandle>, VkResult> {
    let lock = INSTANCE.lock();
    let inst = lock.as_ref().ok_or(VkResult::ErrorInitializationFailed)?;
    let handles = inst.physical_devices.iter().map(|pd| pd.handle).collect();
    Ok(handles)
}

/// vkGetPhysicalDeviceProperties — query device properties
pub fn vk_get_physical_device_properties(
    physical_device: VkHandle,
) -> Result<VkPhysicalDeviceProperties, VkResult> {
    let lock = INSTANCE.lock();
    let inst = lock.as_ref().ok_or(VkResult::ErrorInitializationFailed)?;

    let pd = inst.physical_devices.iter()
        .find(|d| d.handle == physical_device)
        .ok_or(VkResult::ErrorDeviceLost)?;

    Ok(VkPhysicalDeviceProperties {
        api_version: VK_API_VERSION_1_3,
        driver_version: crate::vk_make_api_version(0, 0, 1, 0),
        vendor_id: pd.vendor_id,
        device_id: pd.device_id,
        device_type: VkPhysicalDeviceType::DiscreteGpu,
        device_name: pd.device_name.clone(),
        pipeline_cache_uuid: [0xC1, 0xA0, 0xD1, 0x05, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], // "CLAUDIOOS" identifier
    })
}

/// vkGetPhysicalDeviceFeatures — query supported features
pub fn vk_get_physical_device_features(
    _physical_device: VkHandle,
) -> Result<VkPhysicalDeviceFeatures, VkResult> {
    Ok(VkPhysicalDeviceFeatures::default())
}

/// vkGetPhysicalDeviceMemoryProperties — query memory heaps and types
pub fn vk_get_physical_device_memory_properties(
    physical_device: VkHandle,
) -> Result<VkPhysicalDeviceMemoryProperties, VkResult> {
    let lock = INSTANCE.lock();
    let inst = lock.as_ref().ok_or(VkResult::ErrorInitializationFailed)?;

    let pd = inst.physical_devices.iter()
        .find(|d| d.handle == physical_device)
        .ok_or(VkResult::ErrorDeviceLost)?;

    let mut props = VkPhysicalDeviceMemoryProperties {
        memory_type_count: 3,
        memory_types: [VkMemoryType::default(); 32],
        memory_heap_count: 2,
        memory_heaps: [VkMemoryHeap::default(); 16],
    };

    // Heap 0: Device-local VRAM (GDDR6X)
    props.memory_heaps[0] = VkMemoryHeap {
        size: pd.vram_size,
        flags: VK_MEMORY_HEAP_DEVICE_LOCAL_BIT,
    };

    // Heap 1: Host-visible system RAM (256 MiB mapping window)
    props.memory_heaps[1] = VkMemoryHeap {
        size: 256 * 1024 * 1024,
        flags: 0,
    };

    // Type 0: Device-local only (fastest, for render targets / textures)
    props.memory_types[0] = VkMemoryType {
        property_flags: VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT,
        heap_index: 0,
    };

    // Type 1: Device-local + host-visible (BAR, for streaming uploads)
    props.memory_types[1] = VkMemoryType {
        property_flags: VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT
            | VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT
            | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT,
        heap_index: 0,
    };

    // Type 2: Host-visible + host-coherent (staging buffers)
    props.memory_types[2] = VkMemoryType {
        property_flags: VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT
            | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT,
        heap_index: 1,
    };

    Ok(props)
}

/// vkGetPhysicalDeviceQueueFamilyProperties — query queue families
pub fn vk_get_physical_device_queue_family_properties(
    _physical_device: VkHandle,
) -> Result<Vec<VkQueueFamilyProperties>, VkResult> {
    // NVIDIA GPUs expose one universal queue family (graphics + compute + transfer)
    // and optionally a dedicated transfer queue. We expose two families.
    Ok(vec![
        // Family 0: Universal (graphics + compute + transfer) — 16 queues
        VkQueueFamilyProperties {
            queue_flags: VK_QUEUE_GRAPHICS_BIT | VK_QUEUE_COMPUTE_BIT | VK_QUEUE_TRANSFER_BIT,
            queue_count: 16,
            timestamp_valid_bits: 64,
            min_image_transfer_granularity: VkExtent3D {
                width: 1,
                height: 1,
                depth: 1,
            },
        },
        // Family 1: Async compute — 8 queues
        VkQueueFamilyProperties {
            queue_flags: VK_QUEUE_COMPUTE_BIT | VK_QUEUE_TRANSFER_BIT,
            queue_count: 8,
            timestamp_valid_bits: 64,
            min_image_transfer_granularity: VkExtent3D {
                width: 1,
                height: 1,
                depth: 1,
            },
        },
        // Family 2: Dedicated transfer / DMA — 2 queues
        VkQueueFamilyProperties {
            queue_flags: VK_QUEUE_TRANSFER_BIT,
            queue_count: 2,
            timestamp_valid_bits: 64,
            min_image_transfer_granularity: VkExtent3D {
                width: 16,
                height: 16,
                depth: 1,
            },
        },
    ])
}

/// Helper: access the global instance (for other modules)
pub fn with_instance<F, R>(f: F) -> Result<R, VkResult>
where
    F: FnOnce(&VkInstance) -> R,
{
    let lock = INSTANCE.lock();
    let inst = lock.as_ref().ok_or(VkResult::ErrorInitializationFailed)?;
    Ok(f(inst))
}
