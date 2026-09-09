//! Hand-written, zero-dependency Vulkan renderer for OHOS.
//!
//! Everything goes through dlopen("libvulkan.so") plus vkGetInstanceProcAddr /
//! vkGetDeviceProcAddr; no ash, no wgpu, no build scripts. This is the same path
//! verified on the HarmonyOS PC (3120x2080 swapchain, clear + present).
//!
//! For now the renderer only clears the swapchain image. The GPUI scene pipeline
//! is layered on top of this in later milestones.

#![allow(non_camel_case_types)]

use std::ffi::{CString, c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::GpuSpecs;

pub type LogFn = extern "C" fn(*const c_char);

static LOG: AtomicUsize = AtomicUsize::new(0);

pub fn set_logger(log: LogFn) {
    LOG.store(log as usize, Ordering::Relaxed);
}

pub fn log(msg: &str) {
    let f = LOG.load(Ordering::Relaxed);
    if f == 0 {
        return;
    }
    if let Ok(c) = CString::new(msg) {
        // SAFETY: only ever set from set_logger with a valid extern "C" fn.
        let f: LogFn = unsafe { std::mem::transmute(f) };
        f(c.as_ptr());
    }
}

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
}

const RTLD_NOW: c_int = 2;
const VK_SUCCESS: i32 = 0;
const VK_ERROR_OUT_OF_DATE_KHR: i32 = -1000001004;

const VK_STRUCTURE_TYPE_APPLICATION_INFO: u32 = 0;
const VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO: u32 = 1;
const VK_STRUCTURE_TYPE_SURFACE_CREATE_INFO_OHOS: u32 = 1000685000;
const VK_API_VERSION_1_0: u32 = 1 << 22;
const OHOS_SURFACE_EXT: &str = "VK_OHOS_surface";

const ST_DEVICE_QUEUE_CREATE_INFO: u32 = 2;
const ST_DEVICE_CREATE_INFO: u32 = 3;
const ST_SUBMIT_INFO: u32 = 4;
const ST_FENCE_CREATE_INFO: u32 = 8;
const ST_SEMAPHORE_CREATE_INFO: u32 = 9;
const ST_IMAGE_VIEW_CREATE_INFO: u32 = 15;
const ST_FRAMEBUFFER_CREATE_INFO: u32 = 37;
const ST_RENDER_PASS_CREATE_INFO: u32 = 38;
const ST_COMMAND_POOL_CREATE_INFO: u32 = 39;
const ST_COMMAND_BUFFER_ALLOCATE_INFO: u32 = 40;
const ST_COMMAND_BUFFER_BEGIN_INFO: u32 = 42;
const ST_RENDER_PASS_BEGIN_INFO: u32 = 43;
const ST_SWAPCHAIN_CREATE_INFO_KHR: u32 = 1000001000;
const ST_PRESENT_INFO_KHR: u32 = 1000001001;

const FMT_R8G8B8A8_UNORM: u32 = 37;
const FMT_B8G8R8A8_SRGB: u32 = 50;
const CS_SRGB_NONLINEAR: u32 = 0;
const IMAGE_USAGE_COLOR_ATTACHMENT: u32 = 0x10;
const SHARING_MODE_EXCLUSIVE: u32 = 0;
const COMPOSITE_OPAQUE: u32 = 1;
const PRESENT_MODE_FIFO: u32 = 2;
const PRESENT_MODE_MAILBOX: u32 = 1;
const IMAGE_VIEW_TYPE_2D: u32 = 1;
const LOAD_OP_CLEAR: u32 = 1;
const STORE_OP_STORE: u32 = 0;
const LAYOUT_UNDEFINED: u32 = 0;
const LAYOUT_PRESENT_SRC: u32 = 1000001002;
const LAYOUT_COLOR_ATTACHMENT_OPTIMAL: u32 = 2;
const BIND_POINT_GRAPHICS: u32 = 0;
const QUEUE_GRAPHICS_BIT: u32 = 1;
const CMD_LEVEL_PRIMARY: u32 = 0;
const ASPECT_COLOR: u32 = 1;
const SUBPASS_CONTENTS_INLINE: u32 = 0;
const PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT: u32 = 0x10;
const STENCIL_STORE_OP_DONT_CARE: u32 = 1;

#[repr(C)]
struct VkApplicationInfo {
    s_type: u32,
    p_next: *const c_void,
    p_application_name: *const c_char,
    application_version: u32,
    p_engine_name: *const c_char,
    engine_version: u32,
    api_version: u32,
}

#[repr(C)]
struct VkInstanceCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    p_application_info: *const VkApplicationInfo,
    enabled_layer_count: u32,
    pp_enabled_layer_names: *const *const c_char,
    enabled_extension_count: u32,
    pp_enabled_extension_names: *const *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VkExtent2D {
    width: u32,
    height: u32,
}

#[repr(C)]
struct VkSurfaceCapabilitiesKHR {
    min_image_count: u32,
    max_image_count: u32,
    current_extent: VkExtent2D,
    min_image_extent: VkExtent2D,
    max_image_extent: VkExtent2D,
    max_image_array_layers: u32,
    supported_transforms: u32,
    current_transform: u32,
    supported_composite_alpha: u32,
    supported_usage_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VkSurfaceFormatKHR {
    format: u32,
    color_space: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VkQueueFamilyProperties {
    queue_flags: u32,
    queue_count: u32,
    timestamp_valid_bits: u32,
    min_image_transfer_granularity: [u32; 3],
}

#[repr(C)]
struct VkDeviceQueueCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    queue_family_index: u32,
    queue_count: u32,
    p_queue_priorities: *const f32,
}

#[repr(C)]
struct VkDeviceCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    queue_create_info_count: u32,
    p_queue_create_infos: *const VkDeviceQueueCreateInfo,
    enabled_layer_count: u32,
    pp_enabled_layer_names: *const *const c_char,
    enabled_extension_count: u32,
    pp_enabled_extension_names: *const *const c_char,
    p_enabled_features: *const c_void,
}

#[repr(C)]
struct VkSwapchainCreateInfoKHR {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    surface: u64,
    min_image_count: u32,
    image_format: u32,
    image_color_space: u32,
    image_extent: VkExtent2D,
    image_array_layers: u32,
    image_usage: u32,
    image_sharing_mode: u32,
    queue_family_index_count: u32,
    p_queue_family_indices: *const u32,
    pre_transform: u32,
    composite_alpha: u32,
    present_mode: u32,
    clipped: u32,
    old_swapchain: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VkImageSubresourceRange {
    aspect_mask: u32,
    base_mip_level: u32,
    level_count: u32,
    base_array_layer: u32,
    layer_count: u32,
}

#[repr(C)]
struct VkComponentMapping {
    r: u32,
    g: u32,
    b: u32,
    a: u32,
}

#[repr(C)]
struct VkImageViewCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    image: u64,
    view_type: u32,
    format: u32,
    components: VkComponentMapping,
    subresource_range: VkImageSubresourceRange,
}

#[repr(C)]
struct VkAttachmentReference {
    attachment: u32,
    layout: u32,
}

#[repr(C)]
struct VkAttachmentDescription {
    flags: u32,
    format: u32,
    samples: u32,
    load_op: u32,
    store_op: u32,
    stencil_load_op: u32,
    stencil_store_op: u32,
    initial_layout: u32,
    final_layout: u32,
}

#[repr(C)]
struct VkSubpassDescription {
    flags: u32,
    pipeline_bind_point: u32,
    input_attachment_count: u32,
    p_input_attachments: *const VkAttachmentReference,
    color_attachment_count: u32,
    p_color_attachments: *const VkAttachmentReference,
    p_resolve_attachments: *const VkAttachmentReference,
    p_depth_stencil_attachment: *const VkAttachmentReference,
    preserve_attachment_count: u32,
    p_preserve_attachments: *const u32,
}

#[repr(C)]
struct VkSubpassDependency {
    src_subpass: u32,
    dst_subpass: u32,
    src_stage_mask: u32,
    dst_stage_mask: u32,
    src_access_mask: u32,
    dst_access_mask: u32,
    dependency_flags: u32,
}

#[repr(C)]
struct VkRenderPassCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    attachment_count: u32,
    p_attachments: *const VkAttachmentDescription,
    subpass_count: u32,
    p_subpasses: *const VkSubpassDescription,
    dependency_count: u32,
    p_dependencies: *const c_void,
}

#[repr(C)]
struct VkFramebufferCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    render_pass: u64,
    attachment_count: u32,
    p_attachments: *const u64,
    width: u32,
    height: u32,
    layers: u32,
}

#[repr(C)]
struct VkCommandPoolCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    queue_family_index: u32,
}

#[repr(C)]
struct VkCommandBufferAllocateInfo {
    s_type: u32,
    p_next: *const c_void,
    command_pool: u64,
    level: u32,
    command_buffer_count: u32,
}

#[repr(C)]
struct VkCommandBufferBeginInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    p_inheritance_info: *const c_void,
}

#[repr(C)]
struct VkClearColorValue {
    f: [f32; 4],
}

#[repr(C)]
struct VkClearValue {
    clear_color: VkClearColorValue,
}

#[repr(C)]
struct VkClearAttachment {
    aspect_mask: u32,
    color_attachment: u32,
    clear_value: VkClearValue,
}

#[repr(C)]
struct VkClearRect {
    rect: VkRect2D,
    base_array_layer: u32,
    layer_count: u32,
}

type PFN_vkCmdClearAttachments =
    unsafe extern "C" fn(*mut c_void, u32, *const VkClearAttachment, u32, *const VkClearRect);

/// A solid rectangle painted over the base clear color, in surface pixels.
#[derive(Clone, Copy)]
pub struct ClearRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub color: [f32; 4],
}

#[repr(C)]
struct VkRect2D {
    offset: [i32; 2],
    extent: VkExtent2D,
}

#[repr(C)]
struct VkRenderPassBeginInfo {
    s_type: u32,
    p_next: *const c_void,
    render_pass: u64,
    framebuffer: u64,
    render_area: VkRect2D,
    clear_value_count: u32,
    p_clear_values: *const VkClearValue,
}

#[repr(C)]
struct VkSubmitInfo {
    s_type: u32,
    p_next: *const c_void,
    wait_semaphore_count: u32,
    p_wait_semaphores: *const u64,
    p_wait_dst_stage_mask: *const u32,
    command_buffer_count: u32,
    p_command_buffers: *const *const c_void,
    signal_semaphore_count: u32,
    p_signal_semaphores: *const u64,
}

#[repr(C)]
struct VkPresentInfoKHR {
    s_type: u32,
    p_next: *const c_void,
    wait_semaphore_count: u32,
    p_wait_semaphores: *const u64,
    swapchain_count: u32,
    p_swapchains: *const u64,
    p_image_indices: *const u32,
    p_results: *mut i32,
}

#[repr(C)]
struct VkFenceCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
}

#[repr(C)]
struct VkSemaphoreCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
}

#[repr(C)]
struct VkSurfaceCreateInfoOHOS {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    window: *mut c_void,
}

type PFN_vkGetInstanceProcAddr = unsafe extern "C" fn(*mut c_void, *const c_char) -> *const c_void;
type PFN_vkGetDeviceProcAddr = unsafe extern "C" fn(*mut c_void, *const c_char) -> *const c_void;
type PFN_vkCreateInstance =
    unsafe extern "C" fn(*const VkInstanceCreateInfo, *const c_void, *mut *mut c_void) -> i32;
type PFN_vkEnumeratePhysicalDevices = unsafe extern "C" fn(*mut c_void, *mut u32, *mut u64) -> i32;
type PFN_vkGetPhysicalDeviceQueueFamilyProperties =
    unsafe extern "C" fn(u64, *mut u32, *mut VkQueueFamilyProperties);
type PFN_vkGetPhysicalDeviceSurfaceSupportKHR =
    unsafe extern "C" fn(u64, u32, u64, *mut u32) -> i32;
type PFN_vkGetPhysicalDeviceSurfaceCapabilitiesKHR =
    unsafe extern "C" fn(u64, u64, *mut VkSurfaceCapabilitiesKHR) -> i32;
type PFN_vkGetPhysicalDeviceSurfaceFormatsKHR =
    unsafe extern "C" fn(u64, u64, *mut u32, *mut VkSurfaceFormatKHR) -> i32;
type PFN_vkGetPhysicalDeviceSurfacePresentModesKHR =
    unsafe extern "C" fn(u64, u64, *mut u32, *mut u32) -> i32;
type PFN_vkCreateDevice =
    unsafe extern "C" fn(u64, *const VkDeviceCreateInfo, *const c_void, *mut *mut c_void) -> i32;
type PFN_vkGetDeviceQueue = unsafe extern "C" fn(*mut c_void, u32, u32, *mut *mut c_void);
type PFN_vkDeviceWaitIdle = unsafe extern "C" fn(*mut c_void) -> i32;
type PFN_vkCreateSwapchainKHR = unsafe extern "C" fn(
    *mut c_void,
    *const VkSwapchainCreateInfoKHR,
    *const c_void,
    *mut u64,
) -> i32;
type PFN_vkDestroySwapchainKHR = unsafe extern "C" fn(*mut c_void, u64, *const c_void);
type PFN_vkGetSwapchainImagesKHR =
    unsafe extern "C" fn(*mut c_void, u64, *mut u32, *mut u64) -> i32;
type PFN_vkCreateImageView =
    unsafe extern "C" fn(*mut c_void, *const VkImageViewCreateInfo, *const c_void, *mut u64) -> i32;
type PFN_vkDestroyImageView = unsafe extern "C" fn(*mut c_void, u64, *const c_void);
type PFN_vkCreateRenderPass = unsafe extern "C" fn(
    *mut c_void,
    *const VkRenderPassCreateInfo,
    *const c_void,
    *mut u64,
) -> i32;
type PFN_vkCreateFramebuffer = unsafe extern "C" fn(
    *mut c_void,
    *const VkFramebufferCreateInfo,
    *const c_void,
    *mut u64,
) -> i32;
type PFN_vkDestroyFramebuffer = unsafe extern "C" fn(*mut c_void, u64, *const c_void);
type PFN_vkCreateCommandPool = unsafe extern "C" fn(
    *mut c_void,
    *const VkCommandPoolCreateInfo,
    *const c_void,
    *mut u64,
) -> i32;
type PFN_vkAllocateCommandBuffers =
    unsafe extern "C" fn(*mut c_void, *const VkCommandBufferAllocateInfo, *mut *mut c_void) -> i32;
type PFN_vkResetCommandBuffer = unsafe extern "C" fn(*mut c_void, u32) -> i32;
type PFN_vkBeginCommandBuffer =
    unsafe extern "C" fn(*mut c_void, *const VkCommandBufferBeginInfo) -> i32;
type PFN_vkBeginRenderPass = unsafe extern "C" fn(*mut c_void, *const VkRenderPassBeginInfo, u32);
type PFN_vkCmdEndRenderPass = unsafe extern "C" fn(*mut c_void);
type PFN_vkEndCommandBuffer = unsafe extern "C" fn(*mut c_void) -> i32;
type PFN_vkQueueSubmit = unsafe extern "C" fn(*mut c_void, u32, *const VkSubmitInfo, u64) -> i32;
type PFN_vkQueuePresentKHR = unsafe extern "C" fn(*mut c_void, *const VkPresentInfoKHR) -> i32;
type PFN_vkAcquireNextImageKHR =
    unsafe extern "C" fn(*mut c_void, u64, u64, u64, u64, *mut u32) -> i32;
type PFN_vkCreateFence =
    unsafe extern "C" fn(*mut c_void, *const VkFenceCreateInfo, *const c_void, *mut u64) -> i32;
type PFN_vkWaitForFences = unsafe extern "C" fn(*mut c_void, u32, *const u64, u32, u64) -> i32;
type PFN_vkResetFences = unsafe extern "C" fn(*mut c_void, u32, *const u64) -> i32;
type PFN_vkCreateSemaphore =
    unsafe extern "C" fn(*mut c_void, *const VkSemaphoreCreateInfo, *const c_void, *mut u64) -> i32;
type PFN_vkCreateSurfaceOHOS = unsafe extern "C" fn(
    *mut c_void,
    *const VkSurfaceCreateInfoOHOS,
    *const c_void,
    *mut u64,
) -> i32;

macro_rules! load {
    ($gpd:expr, $device:expr, $name:literal, $ty:ty) => {{
        let cname = CString::new($name).unwrap();
        let p = unsafe { $gpd($device, cname.as_ptr()) };
        if p.is_null() {
            return Err(anyhow::anyhow!(concat!("missing Vulkan symbol: ", $name)));
        }
        unsafe { std::mem::transmute::<*const c_void, $ty>(p) }
    }};
}

struct DeviceFns {
    get_queue: PFN_vkGetDeviceQueue,
    wait_idle: PFN_vkDeviceWaitIdle,
    create_swapchain: PFN_vkCreateSwapchainKHR,
    destroy_swapchain: PFN_vkDestroySwapchainKHR,
    get_swapchain_images: PFN_vkGetSwapchainImagesKHR,
    create_image_view: PFN_vkCreateImageView,
    destroy_image_view: PFN_vkDestroyImageView,
    create_render_pass: PFN_vkCreateRenderPass,
    create_framebuffer: PFN_vkCreateFramebuffer,
    destroy_framebuffer: PFN_vkDestroyFramebuffer,
    create_command_pool: PFN_vkCreateCommandPool,
    alloc_command_buffers: PFN_vkAllocateCommandBuffers,
    reset_command_buffer: PFN_vkResetCommandBuffer,
    begin_command_buffer: PFN_vkBeginCommandBuffer,
    begin_render_pass: PFN_vkBeginRenderPass,
    cmd_end_render_pass: PFN_vkCmdEndRenderPass,
    cmd_clear_attachments: PFN_vkCmdClearAttachments,
    end_command_buffer: PFN_vkEndCommandBuffer,
    queue_submit: PFN_vkQueueSubmit,
    queue_present: PFN_vkQueuePresentKHR,
    acquire_next_image: PFN_vkAcquireNextImageKHR,
    create_fence: PFN_vkCreateFence,
    wait_for_fences: PFN_vkWaitForFences,
    reset_fences: PFN_vkResetFences,
    create_semaphore: PFN_vkCreateSemaphore,
}

/// A persistent Vulkan swapchain renderer bound to one XComponent surface.
pub struct VkRenderer {
    lib: *mut c_void,
    instance: *mut c_void,
    surface: u64,
    physical_device: u64,
    device: *mut c_void,
    queue: *mut c_void,
    queue_family: u32,
    format: u32,
    width: u32,
    height: u32,
    swapchain: u64,
    images: Vec<u64>,
    image_views: Vec<u64>,
    framebuffers: Vec<u64>,
    render_pass: u64,
    command_pool: u64,
    command_buffer: *mut c_void,
    image_available: u64,
    render_finished: u64,
    in_flight: u64,
    fns: DeviceFns,
    gpa: PFN_vkGetInstanceProcAddr,
    gpd: PFN_vkGetDeviceProcAddr,
    text: Option<TextPipeline>,
    quads: Option<QuadPipeline>,
}

impl VkRenderer {
    /// Create the whole Vulkan stack for an OHNativeWindow from an XComponent.
    pub fn new(window: *mut c_void, width: u32, height: u32) -> anyhow::Result<Self> {
        let libname = CString::new("libvulkan.so").unwrap();
        let lib = unsafe { dlopen(libname.as_ptr(), RTLD_NOW) };
        if lib.is_null() {
            anyhow::bail!("dlopen(libvulkan.so) failed");
        }

        let gpa_sym =
            unsafe { dlsym(lib, CString::new("vkGetInstanceProcAddr").unwrap().as_ptr()) };
        if gpa_sym.is_null() {
            anyhow::bail!("dlsym(vkGetInstanceProcAddr) failed");
        }
        let gpa: PFN_vkGetInstanceProcAddr = unsafe { std::mem::transmute(gpa_sym) };

        let khr_ext = CString::new("VK_KHR_surface").unwrap();
        let ohos_ext = CString::new(OHOS_SURFACE_EXT).unwrap();
        let ext_ptrs: [*const c_char; 2] = [khr_ext.as_ptr(), ohos_ext.as_ptr()];
        let app_info = VkApplicationInfo {
            s_type: VK_STRUCTURE_TYPE_APPLICATION_INFO,
            p_next: std::ptr::null(),
            p_application_name: std::ptr::null(),
            application_version: 0,
            p_engine_name: std::ptr::null(),
            engine_version: 0,
            api_version: VK_API_VERSION_1_0,
        };
        let ci = VkInstanceCreateInfo {
            s_type: VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            p_application_info: &app_info,
            enabled_layer_count: 0,
            pp_enabled_layer_names: std::ptr::null(),
            enabled_extension_count: 2,
            pp_enabled_extension_names: ext_ptrs.as_ptr(),
        };
        let create_instance: PFN_vkCreateInstance = unsafe {
            std::mem::transmute(gpa(
                std::ptr::null_mut(),
                CString::new("vkCreateInstance").unwrap().as_ptr(),
            ))
        };
        let mut instance: *mut c_void = std::ptr::null_mut();
        let vr = unsafe { create_instance(&ci, std::ptr::null(), &mut instance) };
        if vr != VK_SUCCESS || instance.is_null() {
            unsafe { dlclose(lib) };
            anyhow::bail!("vkCreateInstance failed: {vr}");
        }

        let cs_raw = unsafe {
            gpa(
                instance,
                CString::new("vkCreateSurfaceOHOS").unwrap().as_ptr(),
            )
        };
        if cs_raw.is_null() {
            anyhow::bail!("vkCreateSurfaceOHOS symbol missing");
        }
        let create_surface: PFN_vkCreateSurfaceOHOS = unsafe { std::mem::transmute(cs_raw) };
        let sci = VkSurfaceCreateInfoOHOS {
            s_type: VK_STRUCTURE_TYPE_SURFACE_CREATE_INFO_OHOS,
            p_next: std::ptr::null(),
            flags: 0,
            window,
        };
        let mut surface: u64 = 0;
        let sr = unsafe { create_surface(instance, &sci, std::ptr::null(), &mut surface) };
        if sr != VK_SUCCESS || surface == 0 {
            anyhow::bail!("vkCreateSurfaceOHOS failed: {sr}");
        }

        let ephys: PFN_vkEnumeratePhysicalDevices = unsafe {
            std::mem::transmute(gpa(
                instance,
                CString::new("vkEnumeratePhysicalDevices").unwrap().as_ptr(),
            ))
        };
        let mut pcount: u32 = 0;
        if unsafe { ephys(instance, &mut pcount, std::ptr::null_mut()) } != VK_SUCCESS
            || pcount == 0
        {
            anyhow::bail!("no physical device");
        }
        let mut phys = vec![0u64; pcount as usize];
        unsafe { ephys(instance, &mut pcount, phys.as_mut_ptr()) };
        let physical_device = phys[0];

        let qfp: PFN_vkGetPhysicalDeviceQueueFamilyProperties = unsafe {
            std::mem::transmute(gpa(
                instance,
                CString::new("vkGetPhysicalDeviceQueueFamilyProperties")
                    .unwrap()
                    .as_ptr(),
            ))
        };
        let mut qcount: u32 = 0;
        unsafe { qfp(physical_device, &mut qcount, std::ptr::null_mut()) };
        let mut qfams = vec![
            VkQueueFamilyProperties {
                queue_flags: 0,
                queue_count: 0,
                timestamp_valid_bits: 0,
                min_image_transfer_granularity: [0; 3],
            };
            qcount as usize
        ];
        unsafe { qfp(physical_device, &mut qcount, qfams.as_mut_ptr()) };
        let ssup: PFN_vkGetPhysicalDeviceSurfaceSupportKHR = unsafe {
            std::mem::transmute(gpa(
                instance,
                CString::new("vkGetPhysicalDeviceSurfaceSupportKHR")
                    .unwrap()
                    .as_ptr(),
            ))
        };
        let mut queue_family = u32::MAX;
        for (i, q) in qfams.iter().enumerate() {
            let mut supported: u32 = 0;
            unsafe { ssup(physical_device, i as u32, surface, &mut supported) };
            if q.queue_flags & QUEUE_GRAPHICS_BIT != 0 && supported != 0 {
                queue_family = i as u32;
                break;
            }
        }
        if queue_family == u32::MAX {
            anyhow::bail!("no graphics+present queue family");
        }

        let sc_ext = CString::new("VK_KHR_swapchain").unwrap();
        let dev_ext: [*const c_char; 1] = [sc_ext.as_ptr()];
        let priorities: [f32; 1] = [1.0];
        let dqci = VkDeviceQueueCreateInfo {
            s_type: ST_DEVICE_QUEUE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            queue_family_index: queue_family,
            queue_count: 1,
            p_queue_priorities: priorities.as_ptr(),
        };
        let dci = VkDeviceCreateInfo {
            s_type: ST_DEVICE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            queue_create_info_count: 1,
            p_queue_create_infos: &dqci,
            enabled_layer_count: 0,
            pp_enabled_layer_names: std::ptr::null(),
            enabled_extension_count: 1,
            pp_enabled_extension_names: dev_ext.as_ptr(),
            p_enabled_features: std::ptr::null(),
        };
        let create_device: PFN_vkCreateDevice = unsafe {
            std::mem::transmute(gpa(
                instance,
                CString::new("vkCreateDevice").unwrap().as_ptr(),
            ))
        };
        let mut device: *mut c_void = std::ptr::null_mut();
        let dr = unsafe { create_device(physical_device, &dci, std::ptr::null(), &mut device) };
        if dr != VK_SUCCESS || device.is_null() {
            anyhow::bail!("vkCreateDevice failed: {dr}");
        }

        let gpd_sym = unsafe { dlsym(lib, CString::new("vkGetDeviceProcAddr").unwrap().as_ptr()) };
        if gpd_sym.is_null() {
            anyhow::bail!("dlsym(vkGetDeviceProcAddr) failed");
        }
        let gpd: PFN_vkGetDeviceProcAddr = unsafe { std::mem::transmute(gpd_sym) };

        let fns = DeviceFns {
            get_queue: load!(gpd, device, "vkGetDeviceQueue", PFN_vkGetDeviceQueue),
            wait_idle: load!(gpd, device, "vkDeviceWaitIdle", PFN_vkDeviceWaitIdle),
            create_swapchain: load!(
                gpd,
                device,
                "vkCreateSwapchainKHR",
                PFN_vkCreateSwapchainKHR
            ),
            destroy_swapchain: load!(
                gpd,
                device,
                "vkDestroySwapchainKHR",
                PFN_vkDestroySwapchainKHR
            ),
            get_swapchain_images: load!(
                gpd,
                device,
                "vkGetSwapchainImagesKHR",
                PFN_vkGetSwapchainImagesKHR
            ),
            create_image_view: load!(gpd, device, "vkCreateImageView", PFN_vkCreateImageView),
            destroy_image_view: load!(gpd, device, "vkDestroyImageView", PFN_vkDestroyImageView),
            create_render_pass: load!(gpd, device, "vkCreateRenderPass", PFN_vkCreateRenderPass),
            create_framebuffer: load!(gpd, device, "vkCreateFramebuffer", PFN_vkCreateFramebuffer),
            destroy_framebuffer: load!(
                gpd,
                device,
                "vkDestroyFramebuffer",
                PFN_vkDestroyFramebuffer
            ),
            create_command_pool: load!(gpd, device, "vkCreateCommandPool", PFN_vkCreateCommandPool),
            alloc_command_buffers: load!(
                gpd,
                device,
                "vkAllocateCommandBuffers",
                PFN_vkAllocateCommandBuffers
            ),
            reset_command_buffer: load!(
                gpd,
                device,
                "vkResetCommandBuffer",
                PFN_vkResetCommandBuffer
            ),
            begin_command_buffer: load!(
                gpd,
                device,
                "vkBeginCommandBuffer",
                PFN_vkBeginCommandBuffer
            ),
            begin_render_pass: load!(gpd, device, "vkCmdBeginRenderPass", PFN_vkBeginRenderPass),
            cmd_end_render_pass: load!(gpd, device, "vkCmdEndRenderPass", PFN_vkCmdEndRenderPass),
            cmd_clear_attachments: load!(
                gpd,
                device,
                "vkCmdClearAttachments",
                PFN_vkCmdClearAttachments
            ),
            end_command_buffer: load!(gpd, device, "vkEndCommandBuffer", PFN_vkEndCommandBuffer),
            queue_submit: load!(gpd, device, "vkQueueSubmit", PFN_vkQueueSubmit),
            queue_present: load!(gpd, device, "vkQueuePresentKHR", PFN_vkQueuePresentKHR),
            acquire_next_image: load!(
                gpd,
                device,
                "vkAcquireNextImageKHR",
                PFN_vkAcquireNextImageKHR
            ),
            create_fence: load!(gpd, device, "vkCreateFence", PFN_vkCreateFence),
            wait_for_fences: load!(gpd, device, "vkWaitForFences", PFN_vkWaitForFences),
            reset_fences: load!(gpd, device, "vkResetFences", PFN_vkResetFences),
            create_semaphore: load!(gpd, device, "vkCreateSemaphore", PFN_vkCreateSemaphore),
        };

        let mut queue: *mut c_void = std::ptr::null_mut();
        unsafe { (fns.get_queue)(device, queue_family, 0, &mut queue) };

        let mut renderer = Self {
            lib,
            instance,
            surface,
            physical_device,
            device,
            queue,
            queue_family,
            format: FMT_R8G8B8A8_UNORM,
            width,
            height,
            swapchain: 0,
            images: Vec::new(),
            image_views: Vec::new(),
            framebuffers: Vec::new(),
            render_pass: 0,
            command_pool: 0,
            command_buffer: std::ptr::null_mut(),
            image_available: 0,
            render_finished: 0,
            in_flight: 0,
            fns,
            gpa,
            gpd,
            text: None,
            quads: None,
        };

        renderer.create_render_pass()?;
        renderer.create_command_pool()?;
        renderer.create_sync_objects()?;
        renderer.create_swapchain()?;
        Ok(renderer)
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn read_gpu_specs(&self) -> anyhow::Result<GpuSpecs> {
        type PFN_vkGetPhysicalDeviceProperties = unsafe extern "C" fn(u64, *mut c_void);
        let proc = self.inst_proc("vkGetPhysicalDeviceProperties");
        if proc.is_null() {
            anyhow::bail!("missing Vulkan symbol: vkGetPhysicalDeviceProperties");
        }
        let get_props: PFN_vkGetPhysicalDeviceProperties = unsafe { std::mem::transmute(proc) };
        // VkPhysicalDeviceProperties is much larger than the fields read here,
        // so a generously sized buffer keeps the driver from writing past it.
        let mut raw = [0u8; 2048];
        unsafe { get_props(self.physical_device, raw.as_mut_ptr() as *mut c_void) };
        let api_version = u32::from_ne_bytes(raw[0..4].try_into().unwrap());
        let driver_version = u32::from_ne_bytes(raw[4..8].try_into().unwrap());
        let vendor_id = u32::from_ne_bytes(raw[8..12].try_into().unwrap());
        let device_id = u32::from_ne_bytes(raw[12..16].try_into().unwrap());
        let device_type = u32::from_ne_bytes(raw[16..20].try_into().unwrap());
        let name_bytes = &raw[20..276];
        let end = name_bytes
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(name_bytes.len());
        let device_name = String::from_utf8_lossy(&name_bytes[..end]).into_owned();
        Ok(GpuSpecs {
            // VK_PHYSICAL_DEVICE_TYPE_CPU = 4
            is_software_emulated: device_type == 4,
            device_name,
            driver_name: format!(
                "Vulkan {}.{}.{}",
                api_version >> 22,
                (api_version >> 12) & 0x3ff,
                api_version & 0xfff
            ),
            driver_info: format!(
                "vendor=0x{vendor_id:04x} device=0x{device_id:04x} driver=0x{driver_version:08x}"
            ),
        })
    }

    pub fn gpu_specs(&self) -> Option<GpuSpecs> {
        self.read_gpu_specs().ok()
    }

    fn create_render_pass(&mut self) -> anyhow::Result<()> {
        let color_ref = VkAttachmentReference {
            attachment: 0,
            layout: LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
        };
        let attachment = VkAttachmentDescription {
            flags: 0,
            format: self.format,
            samples: 1,
            load_op: LOAD_OP_CLEAR,
            store_op: STORE_OP_STORE,
            stencil_load_op: 0,
            stencil_store_op: STENCIL_STORE_OP_DONT_CARE,
            initial_layout: LAYOUT_UNDEFINED,
            final_layout: LAYOUT_PRESENT_SRC,
        };
        let subpass = VkSubpassDescription {
            flags: 0,
            pipeline_bind_point: BIND_POINT_GRAPHICS,
            input_attachment_count: 0,
            p_input_attachments: std::ptr::null(),
            color_attachment_count: 1,
            p_color_attachments: &color_ref,
            p_resolve_attachments: std::ptr::null(),
            p_depth_stencil_attachment: std::ptr::null(),
            preserve_attachment_count: 0,
            p_preserve_attachments: std::ptr::null(),
        };
        let dep = VkSubpassDependency {
            src_subpass: u32::MAX,
            dst_subpass: 0,
            src_stage_mask: PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT,
            dst_stage_mask: PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT,
            src_access_mask: 0,
            dst_access_mask: 0x100,
            dependency_flags: 0,
        };
        let rpci = VkRenderPassCreateInfo {
            s_type: ST_RENDER_PASS_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            attachment_count: 1,
            p_attachments: &attachment,
            subpass_count: 1,
            p_subpasses: &subpass,
            dependency_count: 1,
            p_dependencies: &dep as *const VkSubpassDependency as *const c_void,
        };
        let mut rp: u64 = 0;
        let r =
            unsafe { (self.fns.create_render_pass)(self.device, &rpci, std::ptr::null(), &mut rp) };
        if r != VK_SUCCESS {
            anyhow::bail!("vkCreateRenderPass failed: {r}");
        }
        self.render_pass = rp;
        Ok(())
    }

    fn create_command_pool(&mut self) -> anyhow::Result<()> {
        let cpci = VkCommandPoolCreateInfo {
            s_type: ST_COMMAND_POOL_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            queue_family_index: self.queue_family,
        };
        let mut pool: u64 = 0;
        let r = unsafe {
            (self.fns.create_command_pool)(self.device, &cpci, std::ptr::null(), &mut pool)
        };
        if r != VK_SUCCESS {
            anyhow::bail!("vkCreateCommandPool failed: {r}");
        }
        self.command_pool = pool;
        let cbaci = VkCommandBufferAllocateInfo {
            s_type: ST_COMMAND_BUFFER_ALLOCATE_INFO,
            p_next: std::ptr::null(),
            command_pool: pool,
            level: CMD_LEVEL_PRIMARY,
            command_buffer_count: 1,
        };
        let mut cb: *mut c_void = std::ptr::null_mut();
        let r = unsafe { (self.fns.alloc_command_buffers)(self.device, &cbaci, &mut cb) };
        if r != VK_SUCCESS {
            anyhow::bail!("vkAllocateCommandBuffers failed: {r}");
        }
        self.command_buffer = cb;
        Ok(())
    }

    fn create_sync_objects(&mut self) -> anyhow::Result<()> {
        let fci = VkFenceCreateInfo {
            s_type: ST_FENCE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
        };
        let mut fence: u64 = 0;
        let r = unsafe { (self.fns.create_fence)(self.device, &fci, std::ptr::null(), &mut fence) };
        if r != VK_SUCCESS {
            anyhow::bail!("vkCreateFence failed: {r}");
        }
        self.in_flight = fence;

        let sci = VkSemaphoreCreateInfo {
            s_type: ST_SEMAPHORE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
        };
        let mut img_sem: u64 = 0;
        let mut done_sem: u64 = 0;
        unsafe {
            (self.fns.create_semaphore)(self.device, &sci, std::ptr::null(), &mut img_sem);
            (self.fns.create_semaphore)(self.device, &sci, std::ptr::null(), &mut done_sem);
        }
        self.image_available = img_sem;
        self.render_finished = done_sem;
        Ok(())
    }

    fn create_swapchain(&mut self) -> anyhow::Result<()> {
        let caps_f: PFN_vkGetPhysicalDeviceSurfaceCapabilitiesKHR = unsafe {
            std::mem::transmute((self.gpa)(
                self.instance,
                CString::new("vkGetPhysicalDeviceSurfaceCapabilitiesKHR")
                    .unwrap()
                    .as_ptr(),
            ))
        };
        let fmt_f: PFN_vkGetPhysicalDeviceSurfaceFormatsKHR = unsafe {
            std::mem::transmute((self.gpa)(
                self.instance,
                CString::new("vkGetPhysicalDeviceSurfaceFormatsKHR")
                    .unwrap()
                    .as_ptr(),
            ))
        };
        let mode_f: PFN_vkGetPhysicalDeviceSurfacePresentModesKHR = unsafe {
            std::mem::transmute((self.gpa)(
                self.instance,
                CString::new("vkGetPhysicalDeviceSurfacePresentModesKHR")
                    .unwrap()
                    .as_ptr(),
            ))
        };

        let mut caps = VkSurfaceCapabilitiesKHR {
            min_image_count: 0,
            max_image_count: 0,
            current_extent: VkExtent2D {
                width: 0,
                height: 0,
            },
            min_image_extent: VkExtent2D {
                width: 0,
                height: 0,
            },
            max_image_extent: VkExtent2D {
                width: 0,
                height: 0,
            },
            max_image_array_layers: 0,
            supported_transforms: 0,
            current_transform: 0,
            supported_composite_alpha: 0,
            supported_usage_flags: 0,
        };
        unsafe { caps_f(self.physical_device, self.surface, &mut caps) };

        let mut fcount: u32 = 0;
        unsafe {
            fmt_f(
                self.physical_device,
                self.surface,
                &mut fcount,
                std::ptr::null_mut(),
            )
        };
        let mut fmts = vec![
            VkSurfaceFormatKHR {
                format: FMT_R8G8B8A8_UNORM,
                color_space: CS_SRGB_NONLINEAR,
            };
            fcount.max(1) as usize
        ];
        if fcount > 0 {
            unsafe {
                fmt_f(
                    self.physical_device,
                    self.surface,
                    &mut fcount,
                    fmts.as_mut_ptr(),
                )
            };
        }
        let chosen = fmts
            .iter()
            .find(|f| f.format == FMT_R8G8B8A8_UNORM)
            .or_else(|| fmts.iter().find(|f| f.format == FMT_B8G8R8A8_SRGB))
            .copied()
            .unwrap_or(fmts[0]);
        self.format = chosen.format;

        let mut mcount: u32 = 0;
        unsafe {
            mode_f(
                self.physical_device,
                self.surface,
                &mut mcount,
                std::ptr::null_mut(),
            )
        };
        let mut modes = vec![PRESENT_MODE_FIFO; mcount.max(1) as usize];
        if mcount > 0 {
            unsafe {
                mode_f(
                    self.physical_device,
                    self.surface,
                    &mut mcount,
                    modes.as_mut_ptr(),
                )
            };
        }
        let present_mode = if modes.contains(&PRESENT_MODE_MAILBOX) {
            PRESENT_MODE_MAILBOX
        } else {
            PRESENT_MODE_FIFO
        };

        let (sw, sh) = if caps.current_extent.width != 0 {
            (caps.current_extent.width, caps.current_extent.height)
        } else {
            (self.width, self.height)
        };
        self.width = sw;
        self.height = sh;

        let old = self.swapchain;
        let swci = VkSwapchainCreateInfoKHR {
            s_type: ST_SWAPCHAIN_CREATE_INFO_KHR,
            p_next: std::ptr::null(),
            flags: 0,
            surface: self.surface,
            min_image_count: caps.min_image_count.max(2),
            image_format: chosen.format,
            image_color_space: chosen.color_space,
            image_extent: VkExtent2D {
                width: sw,
                height: sh,
            },
            image_array_layers: 1,
            image_usage: IMAGE_USAGE_COLOR_ATTACHMENT,
            image_sharing_mode: SHARING_MODE_EXCLUSIVE,
            queue_family_index_count: 0,
            p_queue_family_indices: std::ptr::null(),
            pre_transform: caps.current_transform,
            composite_alpha: COMPOSITE_OPAQUE,
            present_mode,
            clipped: 0,
            old_swapchain: old,
        };
        let mut swapchain: u64 = 0;
        let r = unsafe {
            (self.fns.create_swapchain)(self.device, &swci, std::ptr::null(), &mut swapchain)
        };
        if r != VK_SUCCESS {
            anyhow::bail!("vkCreateSwapchainKHR failed: {r}");
        }
        self.swapchain = swapchain;

        let mut imgcount: u32 = 0;
        unsafe {
            (self.fns.get_swapchain_images)(
                self.device,
                swapchain,
                &mut imgcount,
                std::ptr::null_mut(),
            )
        };
        let mut images = vec![0u64; imgcount as usize];
        let r = unsafe {
            (self.fns.get_swapchain_images)(
                self.device,
                swapchain,
                &mut imgcount,
                images.as_mut_ptr(),
            )
        };
        if r != VK_SUCCESS {
            anyhow::bail!("vkGetSwapchainImagesKHR failed: {r}");
        }
        self.images = images.clone();

        self.image_views.clear();
        for &image in &images {
            let ivci = VkImageViewCreateInfo {
                s_type: ST_IMAGE_VIEW_CREATE_INFO,
                p_next: std::ptr::null(),
                flags: 0,
                image,
                view_type: IMAGE_VIEW_TYPE_2D,
                format: self.format,
                components: VkComponentMapping {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 0,
                },
                subresource_range: VkImageSubresourceRange {
                    aspect_mask: ASPECT_COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                },
            };
            let mut view: u64 = 0;
            let r = unsafe {
                (self.fns.create_image_view)(self.device, &ivci, std::ptr::null(), &mut view)
            };
            if r != VK_SUCCESS {
                anyhow::bail!("vkCreateImageView failed: {r}");
            }
            self.image_views.push(view);
        }

        self.framebuffers.clear();
        for &view in &self.image_views {
            let fbci = VkFramebufferCreateInfo {
                s_type: ST_FRAMEBUFFER_CREATE_INFO,
                p_next: std::ptr::null(),
                flags: 0,
                render_pass: self.render_pass,
                attachment_count: 1,
                p_attachments: &view,
                width: sw,
                height: sh,
                layers: 1,
            };
            let mut fb: u64 = 0;
            let r = unsafe {
                (self.fns.create_framebuffer)(self.device, &fbci, std::ptr::null(), &mut fb)
            };
            if r != VK_SUCCESS {
                anyhow::bail!("vkCreateFramebuffer failed: {r}");
            }
            self.framebuffers.push(fb);
        }
        Ok(())
    }

    fn destroy_swapchain_resources(&mut self) {
        for &fb in &self.framebuffers {
            unsafe { (self.fns.destroy_framebuffer)(self.device, fb, std::ptr::null()) };
        }
        self.framebuffers.clear();
        for &view in &self.image_views {
            unsafe { (self.fns.destroy_image_view)(self.device, view, std::ptr::null()) };
        }
        self.image_views.clear();
        if self.swapchain != 0 {
            unsafe { (self.fns.destroy_swapchain)(self.device, self.swapchain, std::ptr::null()) };
            self.swapchain = 0;
        }
    }

    /// Recreate the swapchain for a new surface size.
    pub fn resize(&mut self, width: u32, height: u32) -> anyhow::Result<()> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        if (self.width, self.height) == (width, height) && self.swapchain != 0 {
            return Ok(());
        }
        unsafe { (self.fns.wait_idle)(self.device) };
        self.destroy_swapchain_resources();
        self.width = width;
        self.height = height;
        self.create_swapchain()
    }

    /// Clear the swapchain to color and present it.
    #[allow(dead_code)] // the documented swapchain clear path; scenes use render_scene
    pub fn render_clear(&mut self, color: [f32; 4]) -> anyhow::Result<()> {
        self.render_frame(color, &[])
    }

    /// Clear the swapchain to color, then paint rects in order on top.
    #[allow(dead_code)] // see render_clear
    pub fn render_frame(&mut self, color: [f32; 4], rects: &[ClearRect]) -> anyhow::Result<()> {
        unsafe {
            (self.fns.wait_for_fences)(self.device, 1, &self.in_flight, 1, u64::MAX);
            (self.fns.reset_fences)(self.device, 1, &self.in_flight);
        }

        let mut image_index: u32 = 0;
        let ar = unsafe {
            (self.fns.acquire_next_image)(
                self.device,
                self.swapchain,
                u64::MAX,
                self.image_available,
                0,
                &mut image_index,
            )
        };
        if ar == VK_ERROR_OUT_OF_DATE_KHR {
            let (w, h) = (self.width, self.height);
            self.resize(w, h)?;
            return Ok(());
        }
        if ar != VK_SUCCESS {
            anyhow::bail!("vkAcquireNextImageKHR failed: {ar}");
        }

        unsafe { (self.fns.reset_command_buffer)(self.command_buffer, 0) };
        let cbbi = VkCommandBufferBeginInfo {
            s_type: ST_COMMAND_BUFFER_BEGIN_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            p_inheritance_info: std::ptr::null(),
        };
        let r = unsafe { (self.fns.begin_command_buffer)(self.command_buffer, &cbbi) };
        if r != VK_SUCCESS {
            anyhow::bail!("vkBeginCommandBuffer failed: {r}");
        }

        let clear = VkClearValue {
            clear_color: VkClearColorValue { f: color },
        };
        let area = VkRect2D {
            offset: [0, 0],
            extent: VkExtent2D {
                width: self.width,
                height: self.height,
            },
        };
        let rpbi = VkRenderPassBeginInfo {
            s_type: ST_RENDER_PASS_BEGIN_INFO,
            p_next: std::ptr::null(),
            render_pass: self.render_pass,
            framebuffer: self.framebuffers[image_index as usize],
            render_area: area,
            clear_value_count: 1,
            p_clear_values: &clear,
        };
        unsafe {
            (self.fns.begin_render_pass)(self.command_buffer, &rpbi, SUBPASS_CONTENTS_INLINE);
            for rect in rects {
                self.cmd_clear_rect(*rect);
            }
            (self.fns.cmd_end_render_pass)(self.command_buffer);
            (self.fns.end_command_buffer)(self.command_buffer);
        }

        let wait_stage = [PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT];
        let cmds: [*const c_void; 1] = [self.command_buffer];
        let si = VkSubmitInfo {
            s_type: ST_SUBMIT_INFO,
            p_next: std::ptr::null(),
            wait_semaphore_count: 1,
            p_wait_semaphores: &self.image_available,
            p_wait_dst_stage_mask: wait_stage.as_ptr(),
            command_buffer_count: 1,
            p_command_buffers: cmds.as_ptr(),
            signal_semaphore_count: 1,
            p_signal_semaphores: &self.render_finished,
        };
        let r = unsafe { (self.fns.queue_submit)(self.queue, 1, &si, self.in_flight) };
        if r != VK_SUCCESS {
            anyhow::bail!("vkQueueSubmit failed: {r}");
        }
        unsafe { (self.fns.wait_for_fences)(self.device, 1, &self.in_flight, 1, u64::MAX) };

        let pi = VkPresentInfoKHR {
            s_type: ST_PRESENT_INFO_KHR,
            p_next: std::ptr::null(),
            wait_semaphore_count: 1,
            p_wait_semaphores: &self.render_finished,
            swapchain_count: 1,
            p_swapchains: &self.swapchain,
            p_image_indices: &image_index,
            p_results: std::ptr::null_mut(),
        };
        let pr = unsafe { (self.fns.queue_present)(self.queue, &pi) };
        if pr == VK_ERROR_OUT_OF_DATE_KHR {
            let (w, h) = (self.width, self.height);
            self.resize(w, h)?;
            return Ok(());
        }
        if pr != VK_SUCCESS {
            anyhow::bail!("vkQueuePresentKHR failed: {pr}");
        }
        Ok(())
    }

    /// Paint one clipped solid rectangle inside the active render pass.
    fn cmd_clear_rect(&self, rect: ClearRect) {
        let x = rect.x.max(0);
        let y = rect.y.max(0);
        let x1 = rect
            .x
            .saturating_add(rect.width as i32)
            .min(self.width as i32);
        let y1 = rect
            .y
            .saturating_add(rect.height as i32)
            .min(self.height as i32);
        if x1 <= x || y1 <= y {
            return;
        }
        let attachment = VkClearAttachment {
            aspect_mask: ASPECT_COLOR,
            color_attachment: 0,
            clear_value: VkClearValue {
                clear_color: VkClearColorValue { f: rect.color },
            },
        };
        let clear_rect = VkClearRect {
            rect: VkRect2D {
                offset: [x, y],
                extent: VkExtent2D {
                    width: (x1 - x) as u32,
                    height: (y1 - y) as u32,
                },
            },
            base_array_layer: 0,
            layer_count: 1,
        };
        unsafe {
            (self.fns.cmd_clear_attachments)(self.command_buffer, 1, &attachment, 1, &clear_rect);
        }
    }
}

impl Drop for VkRenderer {
    fn drop(&mut self) {
        unsafe {
            (self.fns.wait_idle)(self.device);
        }
        self.destroy_swapchain_resources();
        unsafe {
            if !self.lib.is_null() {
                dlclose(self.lib);
            }
        }
    }
}

// ===========================================================================
// Software compositing path (M2 text)
//
// Instead of a full descriptor/pipeline setup, the scene is composited on the
// CPU (solid quads plus glyph coverage) and uploaded to the swapchain image
// through a host-visible staging buffer. Static frames are skipped by hash.
// ===========================================================================

const ST_BUFFER_CREATE_INFO: u32 = 12;
const ST_MEMORY_ALLOCATE_INFO: u32 = 5;
const ST_IMAGE_MEMORY_BARRIER: u32 = 45;
const BUFFER_USAGE_TRANSFER_SRC: u32 = 0x0001;
const BUFFER_USAGE_VERTEX_BUFFER: u32 = 0x0080;
const MEMORY_PROPERTY_HOST_VISIBLE: u32 = 0x0002;
const MEMORY_PROPERTY_HOST_COHERENT: u32 = 0x0004;
const LAYOUT_TRANSFER_DST_OPTIMAL: u32 = 7;
const PIPELINE_STAGE_TRANSFER: u32 = 0x1000;
const ACCESS_TRANSFER_WRITE: u32 = 0x1000;
const QUEUE_FAMILY_IGNORED: u32 = 0xFFFF_FFFF;
const VK_WHOLE_SIZE: u64 = !0u64;

macro_rules! inst_fn {
    ($renderer:expr, $name:literal, $ty:ty) => {{
        let p = $renderer.inst_proc($name);
        if p.is_null() {
            anyhow::bail!(concat!("missing Vulkan symbol: ", $name));
        }
        unsafe { std::mem::transmute::<*const c_void, $ty>(p) }
    }};
}

macro_rules! dev_fn {
    ($renderer:expr, $name:literal, $ty:ty) => {{
        let p = $renderer.dev_proc($name);
        if p.is_null() {
            anyhow::bail!(concat!("missing Vulkan symbol: ", $name));
        }
        unsafe { std::mem::transmute::<*const c_void, $ty>(p) }
    }};
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VkMemoryRequirements {
    size: u64,
    alignment: u64,
    memory_type_bits: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VkMemoryType {
    property_flags: u32,
    heap_index: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VkMemoryHeap {
    size: u64,
    flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VkPhysicalDeviceMemoryProperties {
    memory_type_count: u32,
    memory_types: [VkMemoryType; 32],
    memory_heap_count: u32,
    memory_heaps: [VkMemoryHeap; 16],
}

#[repr(C)]
struct VkBufferCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    size: u64,
    usage: u32,
    sharing_mode: u32,
    queue_family_index_count: u32,
    p_queue_family_indices: *const u32,
}

#[repr(C)]
struct VkMemoryAllocateInfo {
    s_type: u32,
    p_next: *const c_void,
    allocation_size: u64,
    memory_type_index: u32,
}

#[repr(C)]
struct VkImageSubresourceLayers {
    aspect_mask: u32,
    mip_level: u32,
    base_array_layer: u32,
    layer_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VkOffset3D {
    x: i32,
    y: i32,
    z: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VkExtent3D {
    width: u32,
    height: u32,
    depth: u32,
}

#[repr(C)]
struct VkBufferImageCopy {
    buffer_offset: u64,
    buffer_row_length: u32,
    buffer_image_height: u32,
    image_subresource: VkImageSubresourceLayers,
    image_offset: VkOffset3D,
    image_extent: VkExtent3D,
}

#[repr(C)]
struct VkImageMemoryBarrier {
    s_type: u32,
    p_next: *const c_void,
    src_access_mask: u32,
    dst_access_mask: u32,
    old_layout: u32,
    new_layout: u32,
    src_queue_family_index: u32,
    dst_queue_family_index: u32,
    image: u64,
    subresource_range: VkImageSubresourceRange,
}

type PFN_vkGetPhysicalDeviceMemoryProperties =
    unsafe extern "C" fn(u64, *mut VkPhysicalDeviceMemoryProperties);
type PFN_vkCreateBuffer =
    unsafe extern "C" fn(*mut c_void, *const VkBufferCreateInfo, *const c_void, *mut u64) -> i32;
type PFN_vkGetBufferMemoryRequirements =
    unsafe extern "C" fn(*mut c_void, u64, *mut VkMemoryRequirements);
type PFN_vkAllocateMemory =
    unsafe extern "C" fn(*mut c_void, *const VkMemoryAllocateInfo, *const c_void, *mut u64) -> i32;
type PFN_vkBindBufferMemory = unsafe extern "C" fn(*mut c_void, u64, u64, u64) -> i32;
type PFN_vkMapMemory =
    unsafe extern "C" fn(*mut c_void, u64, u64, u64, u32, *mut *mut c_void) -> i32;
type PFN_vkCmdCopyBufferToImage =
    unsafe extern "C" fn(*mut c_void, u64, u64, u32, u32, *const VkBufferImageCopy);
type PFN_vkCmdPipelineBarrier = unsafe extern "C" fn(
    *mut c_void,
    u32,
    u32,
    u32,
    u32,
    *const c_void,
    u32,
    *const c_void,
    u32,
    *const VkImageMemoryBarrier,
);

impl VkRenderer {
    fn inst_proc(&self, name: &str) -> *const c_void {
        unsafe { (self.gpa)(self.instance, CString::new(name).unwrap().as_ptr()) }
    }

    fn dev_proc(&self, name: &str) -> *const c_void {
        unsafe { (self.gpd)(self.device, CString::new(name).unwrap().as_ptr()) }
    }

    fn find_memory_type(&self, type_bits: u32, required: u32) -> anyhow::Result<u32> {
        let get_props: PFN_vkGetPhysicalDeviceMemoryProperties = inst_fn!(
            self,
            "vkGetPhysicalDeviceMemoryProperties",
            PFN_vkGetPhysicalDeviceMemoryProperties
        );
        let mut props: VkPhysicalDeviceMemoryProperties = unsafe { std::mem::zeroed() };
        unsafe { get_props(self.physical_device, &mut props) };
        for i in 0..props.memory_type_count {
            let ty = props.memory_types[i as usize];
            if type_bits & (1 << i) != 0 && ty.property_flags & required == required {
                return Ok(i);
            }
        }
        anyhow::bail!("no suitable Vulkan memory type (required {required:#x})")
    }
}

// ===========================================================================
// GPU glyph pipeline (textured quads from the monochrome atlas)
// ===========================================================================

const ST_IMAGE_CREATE_INFO: u32 = 14;
const ST_SAMPLER_CREATE_INFO: u32 = 31;
const ST_DESCRIPTOR_SET_LAYOUT_CREATE_INFO: u32 = 32;
const ST_DESCRIPTOR_POOL_CREATE_INFO: u32 = 33;
const ST_DESCRIPTOR_SET_ALLOCATE_INFO: u32 = 34;
const ST_WRITE_DESCRIPTOR_SET: u32 = 35;
const ST_PIPELINE_LAYOUT_CREATE_INFO: u32 = 30;
const ST_SHADER_MODULE_CREATE_INFO: u32 = 16;
const ST_PIPELINE_SHADER_STAGE_CREATE_INFO: u32 = 18;
const ST_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO: u32 = 19;
const ST_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO: u32 = 20;
const ST_PIPELINE_VIEWPORT_STATE_CREATE_INFO: u32 = 22;
const ST_PIPELINE_RASTERIZATION_STATE_CREATE_INFO: u32 = 23;
const ST_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO: u32 = 24;
const ST_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO: u32 = 26;
const ST_PIPELINE_DYNAMIC_STATE_CREATE_INFO: u32 = 27;
const ST_GRAPHICS_PIPELINE_CREATE_INFO: u32 = 28;
const IMAGE_TYPE_2D: u32 = 1;
const IMAGE_TILING_OPTIMAL: u32 = 0;
const IMAGE_USAGE_TRANSFER_DST: u32 = 0x0002;
const IMAGE_USAGE_SAMPLED: u32 = 0x0004;
const FORMAT_R8_UNORM: u32 = 9;
const SAMPLE_COUNT_1: u32 = 1;
const SHADER_STAGE_VERTEX: u32 = 0x1;
const SHADER_STAGE_FRAGMENT: u32 = 0x10;
const DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER: u32 = 1;
const FILTER_LINEAR: u32 = 1;
const SAMPLER_MIPMAP_MODE_NEAREST: u32 = 0;
const SAMPLER_ADDRESS_CLAMP_TO_EDGE: u32 = 2;
const BORDER_COLOR_FLOAT_OPAQUE_WHITE: u32 = 1;
const DYNAMIC_STATE_VIEWPORT: u32 = 0;
const DYNAMIC_STATE_SCISSOR: u32 = 1;
const TOPOLOGY_TRIANGLE_LIST: u32 = 3;
const POLYGON_MODE_FILL: u32 = 0;
const CULL_MODE_NONE: u32 = 0;
const FRONT_FACE_COUNTER_CLOCKWISE: u32 = 0;
const BLEND_FACTOR_SRC_ALPHA: u32 = 6;
const BLEND_FACTOR_ONE_MINUS_SRC_ALPHA: u32 = 7;
const BLEND_OP_ADD: u32 = 0;
const COLOR_COMPONENT_RGBA: u32 = 0xF;
const VERTEX_INPUT_RATE_VERTEX: u32 = 0;
const FORMAT_R32G32_SFLOAT: u32 = 103;
const FORMAT_R32G32B32A32_SFLOAT: u32 = 109;
const LAYOUT_SHADER_READ_ONLY_OPTIMAL: u32 = 5;
const ACCESS_SHADER_READ: u32 = 0x20;
const PIPELINE_STAGE_FRAGMENT_SHADER: u32 = 0x80;

#[repr(C)]
struct VkImageCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    image_type: u32,
    format: u32,
    extent: VkExtent3D,
    mip_levels: u32,
    array_layers: u32,
    samples: u32,
    tiling: u32,
    usage: u32,
    sharing_mode: u32,
    queue_family_index_count: u32,
    p_queue_family_indices: *const u32,
    initial_layout: u32,
}

#[repr(C)]
struct VkSamplerCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    mag_filter: u32,
    min_filter: u32,
    mipmap_mode: u32,
    address_mode_u: u32,
    address_mode_v: u32,
    address_mode_w: u32,
    mip_lod_bias: f32,
    anisotropy_enable: u32,
    max_anisotropy: f32,
    compare_enable: u32,
    compare_op: u32,
    min_lod: f32,
    max_lod: f32,
    border_color: u32,
    unnormalized_coordinates: u32,
}

#[repr(C)]
struct VkDescriptorSetLayoutBinding {
    binding: u32,
    descriptor_type: u32,
    descriptor_count: u32,
    stage_flags: u32,
    p_immutable_samplers: *const u64,
}

#[repr(C)]
struct VkDescriptorSetLayoutCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    binding_count: u32,
    p_bindings: *const VkDescriptorSetLayoutBinding,
}

#[repr(C)]
struct VkDescriptorPoolSize {
    ty: u32,
    descriptor_count: u32,
}

#[repr(C)]
struct VkDescriptorPoolCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    max_sets: u32,
    pool_size_count: u32,
    p_pool_sizes: *const VkDescriptorPoolSize,
}

#[repr(C)]
struct VkDescriptorSetAllocateInfo {
    s_type: u32,
    p_next: *const c_void,
    descriptor_pool: u64,
    descriptor_set_count: u32,
    p_set_layouts: *const u64,
}

#[repr(C)]
struct VkDescriptorImageInfo {
    sampler: u64,
    image_view: u64,
    image_layout: u32,
}

#[repr(C)]
struct VkWriteDescriptorSet {
    s_type: u32,
    p_next: *const c_void,
    dst_set: u64,
    dst_binding: u32,
    dst_array_element: u32,
    descriptor_count: u32,
    descriptor_type: u32,
    p_image_info: *const VkDescriptorImageInfo,
    p_buffer_info: *const c_void,
    p_texel_buffer_view: *const c_void,
}

#[repr(C)]
struct VkPipelineLayoutCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    set_layout_count: u32,
    p_set_layouts: *const u64,
    push_constant_range_count: u32,
    p_push_constant_ranges: *const c_void,
}

#[repr(C)]
struct VkShaderModuleCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    code_size: usize,
    p_code: *const u32,
}

#[repr(C)]
struct VkPipelineShaderStageCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    stage: u32,
    module: u64,
    p_name: *const c_char,
    p_specialization_info: *const c_void,
}

#[repr(C)]
struct VkVertexInputBindingDescription {
    binding: u32,
    stride: u32,
    input_rate: u32,
}

#[repr(C)]
struct VkVertexInputAttributeDescription {
    location: u32,
    binding: u32,
    format: u32,
    offset: u32,
}

#[repr(C)]
struct VkPipelineVertexInputStateCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    vertex_binding_description_count: u32,
    p_vertex_binding_descriptions: *const VkVertexInputBindingDescription,
    vertex_attribute_description_count: u32,
    p_vertex_attribute_descriptions: *const VkVertexInputAttributeDescription,
}

#[repr(C)]
struct VkPipelineInputAssemblyStateCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    topology: u32,
    primitive_restart_enable: u32,
}

#[repr(C)]
struct VkViewport {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    min_depth: f32,
    max_depth: f32,
}

#[repr(C)]
struct VkPipelineViewportStateCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    viewport_count: u32,
    p_viewports: *const VkViewport,
    scissor_count: u32,
    p_scissors: *const VkRect2D,
}

#[repr(C)]
struct VkPipelineRasterizationStateCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    depth_clamp_enable: u32,
    rasterizer_discard_enable: u32,
    polygon_mode: u32,
    cull_mode: u32,
    front_face: u32,
    depth_bias_enable: u32,
    depth_bias_constant_factor: f32,
    depth_bias_clamp: f32,
    depth_bias_slope_factor: f32,
    line_width: f32,
}

#[repr(C)]
struct VkPipelineMultisampleStateCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    rasterization_samples: u32,
    sample_shading_enable: u32,
    min_sample_shading: f32,
    p_sample_mask: *const u32,
    alpha_to_coverage_enable: u32,
    alpha_to_one_enable: u32,
}

#[repr(C)]
struct VkPipelineColorBlendAttachmentState {
    blend_enable: u32,
    src_color_blend_factor: u32,
    dst_color_blend_factor: u32,
    color_blend_op: u32,
    src_alpha_blend_factor: u32,
    dst_alpha_blend_factor: u32,
    alpha_blend_op: u32,
    color_write_mask: u32,
}

#[repr(C)]
struct VkPipelineColorBlendStateCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    logic_op_enable: u32,
    logic_op: u32,
    attachment_count: u32,
    p_attachments: *const VkPipelineColorBlendAttachmentState,
    blend_constants: [f32; 4],
}

#[repr(C)]
struct VkPipelineDynamicStateCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    dynamic_state_count: u32,
    p_dynamic_states: *const u32,
}

#[repr(C)]
struct VkGraphicsPipelineCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    stage_count: u32,
    p_stages: *const VkPipelineShaderStageCreateInfo,
    p_vertex_input_state: *const VkPipelineVertexInputStateCreateInfo,
    p_input_assembly_state: *const VkPipelineInputAssemblyStateCreateInfo,
    p_tessellation_state: *const c_void,
    p_viewport_state: *const VkPipelineViewportStateCreateInfo,
    p_rasterization_state: *const VkPipelineRasterizationStateCreateInfo,
    p_multisample_state: *const VkPipelineMultisampleStateCreateInfo,
    p_depth_stencil_state: *const c_void,
    p_color_blend_state: *const VkPipelineColorBlendStateCreateInfo,
    p_dynamic_state: *const VkPipelineDynamicStateCreateInfo,
    layout: u64,
    render_pass: u64,
    subpass: u32,
    base_pipeline_handle: u64,
    base_pipeline_index: i32,
}

type PFN_vkCreateImage =
    unsafe extern "C" fn(*mut c_void, *const VkImageCreateInfo, *const c_void, *mut u64) -> i32;
type PFN_vkGetImageMemoryRequirements =
    unsafe extern "C" fn(*mut c_void, u64, *mut VkMemoryRequirements);
type PFN_vkBindImageMemory = unsafe extern "C" fn(*mut c_void, u64, u64, u64) -> i32;
type PFN_vkCreateSampler =
    unsafe extern "C" fn(*mut c_void, *const VkSamplerCreateInfo, *const c_void, *mut u64) -> i32;
type PFN_vkCreateDescriptorSetLayout = unsafe extern "C" fn(
    *mut c_void,
    *const VkDescriptorSetLayoutCreateInfo,
    *const c_void,
    *mut u64,
) -> i32;
type PFN_vkCreateDescriptorPool = unsafe extern "C" fn(
    *mut c_void,
    *const VkDescriptorPoolCreateInfo,
    *const c_void,
    *mut u64,
) -> i32;
type PFN_vkAllocateDescriptorSets =
    unsafe extern "C" fn(*mut c_void, *const VkDescriptorSetAllocateInfo, *mut u64) -> i32;
type PFN_vkUpdateDescriptorSets =
    unsafe extern "C" fn(*mut c_void, u32, *const VkWriteDescriptorSet, u32, *const c_void);
type PFN_vkCreatePipelineLayout = unsafe extern "C" fn(
    *mut c_void,
    *const VkPipelineLayoutCreateInfo,
    *const c_void,
    *mut u64,
) -> i32;
type PFN_vkCreateShaderModule = unsafe extern "C" fn(
    *mut c_void,
    *const VkShaderModuleCreateInfo,
    *const c_void,
    *mut u64,
) -> i32;
type PFN_vkCreateGraphicsPipelines = unsafe extern "C" fn(
    *mut c_void,
    u64,
    u32,
    *const VkGraphicsPipelineCreateInfo,
    *const c_void,
    *mut u64,
) -> i32;
type PFN_vkCmdBindPipeline = unsafe extern "C" fn(*mut c_void, u32, u64);
type PFN_vkCmdBindVertexBuffers =
    unsafe extern "C" fn(*mut c_void, u32, u32, *const u64, *const u64);
type PFN_vkCmdBindDescriptorSets =
    unsafe extern "C" fn(*mut c_void, u32, u64, u32, u32, *const u64, u32, *const u32);
type PFN_vkCmdDraw = unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32);
type PFN_vkCmdSetViewport = unsafe extern "C" fn(*mut c_void, u32, u32, *const VkViewport);
type PFN_vkCmdSetScissor = unsafe extern "C" fn(*mut c_void, u32, u32, *const VkRect2D);

/// Vertex layout shared with the shader: the fields are written into the GPU
/// buffer and never read back on the CPU.
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct GlyphVertex {
    pub x: f32,
    pub y: f32,
    pub u: f32,
    pub v: f32,
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

/// Rounded-rectangle vertex: NDC position, local offset and half extents in
/// device pixels, per-corner radii, border width and both colours.
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct QuadVertex {
    pub x: f32,
    pub y: f32,
    pub local_x: f32,
    pub local_y: f32,
    pub half_width: f32,
    pub half_height: f32,
    pub radius_tl: f32,
    pub radius_tr: f32,
    pub radius_br: f32,
    pub radius_bl: f32,
    pub border: f32,
    pub pad: f32,
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
    pub border_r: f32,
    pub border_g: f32,
    pub border_b: f32,
    pub border_a: f32,
}

/// Rounded-rectangle pipeline: no descriptors, only a vertex buffer.
struct QuadPipeline {
    /// Held so the pipeline's layout outlives it.
    #[allow(dead_code)]
    pipeline_layout: u64,
    pipeline: u64,
    vertex_buffer: u64,
    #[allow(dead_code)]
    vertex_memory: u64,
    vertex_mapped: *mut u8,
    vertex_size: u64,
}

/// Owns the descriptor set, image view and sampler: the handles are held to
/// keep the Vulkan objects alive, not read back.
#[allow(dead_code)]
struct TextPipeline {
    descriptor_set_layout: u64,
    descriptor_pool: u64,
    descriptor_set: u64,
    pipeline_layout: u64,
    pipeline: u64,
    image: u64,
    #[allow(dead_code)]
    image_memory: u64,
    view: u64,
    sampler: u64,
    vertex_buffer: u64,
    #[allow(dead_code)]
    vertex_memory: u64,
    vertex_mapped: *mut u8,
    vertex_size: u64,
    atlas_extent: (u32, u32),
}

fn load_spirv() -> &'static [u32] {
    const BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/glyph.spv"));
    // The array is 4-byte aligned because it is a static byte slice of a
    // multiple-of-4 length; reinterpret as u32 words.
    unsafe { std::slice::from_raw_parts(BYTES.as_ptr() as *const u32, BYTES.len() / 4) }
}

fn load_quad_spirv() -> &'static [u32] {
    const BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/quad.spv"));
    unsafe { std::slice::from_raw_parts(BYTES.as_ptr() as *const u32, BYTES.len() / 4) }
}

impl VkRenderer {
    fn create_text_pipeline(&mut self) -> anyhow::Result<()> {
        let create_image: PFN_vkCreateImage = dev_fn!(self, "vkCreateImage", PFN_vkCreateImage);
        let image_reqs: PFN_vkGetImageMemoryRequirements = dev_fn!(
            self,
            "vkGetImageMemoryRequirements",
            PFN_vkGetImageMemoryRequirements
        );
        let bind_image: PFN_vkBindImageMemory =
            dev_fn!(self, "vkBindImageMemory", PFN_vkBindImageMemory);
        let create_view: PFN_vkCreateImageView =
            dev_fn!(self, "vkCreateImageView", PFN_vkCreateImageView);
        let create_sampler: PFN_vkCreateSampler =
            dev_fn!(self, "vkCreateSampler", PFN_vkCreateSampler);
        let alloc_mem: PFN_vkAllocateMemory =
            inst_fn!(self, "vkAllocateMemory", PFN_vkAllocateMemory);
        let create_set_layout: PFN_vkCreateDescriptorSetLayout = dev_fn!(
            self,
            "vkCreateDescriptorSetLayout",
            PFN_vkCreateDescriptorSetLayout
        );
        let create_pool: PFN_vkCreateDescriptorPool =
            dev_fn!(self, "vkCreateDescriptorPool", PFN_vkCreateDescriptorPool);
        let alloc_sets: PFN_vkAllocateDescriptorSets = dev_fn!(
            self,
            "vkAllocateDescriptorSets",
            PFN_vkAllocateDescriptorSets
        );
        let update_sets: PFN_vkUpdateDescriptorSets =
            dev_fn!(self, "vkUpdateDescriptorSets", PFN_vkUpdateDescriptorSets);
        let create_layout: PFN_vkCreatePipelineLayout =
            dev_fn!(self, "vkCreatePipelineLayout", PFN_vkCreatePipelineLayout);
        let create_shader: PFN_vkCreateShaderModule =
            dev_fn!(self, "vkCreateShaderModule", PFN_vkCreateShaderModule);
        let create_pipelines: PFN_vkCreateGraphicsPipelines = dev_fn!(
            self,
            "vkCreateGraphicsPipelines",
            PFN_vkCreateGraphicsPipelines
        );

        const ATLAS: u32 = 2048;
        let ici = VkImageCreateInfo {
            s_type: ST_IMAGE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            image_type: IMAGE_TYPE_2D,
            format: FORMAT_R8_UNORM,
            extent: VkExtent3D {
                width: ATLAS,
                height: ATLAS,
                depth: 1,
            },
            mip_levels: 1,
            array_layers: 1,
            samples: SAMPLE_COUNT_1,
            tiling: IMAGE_TILING_OPTIMAL,
            usage: IMAGE_USAGE_TRANSFER_DST | IMAGE_USAGE_SAMPLED,
            sharing_mode: SHARING_MODE_EXCLUSIVE,
            queue_family_index_count: 0,
            p_queue_family_indices: std::ptr::null(),
            initial_layout: LAYOUT_UNDEFINED,
        };
        let mut image: u64 = 0;
        if unsafe { create_image(self.device, &ici, std::ptr::null(), &mut image) } != VK_SUCCESS {
            anyhow::bail!("vkCreateImage failed");
        }
        let mut reqs = VkMemoryRequirements {
            size: 0,
            alignment: 0,
            memory_type_bits: 0,
        };
        unsafe { image_reqs(self.device, image, &mut reqs) };
        let memory_type = self.find_memory_type(reqs.memory_type_bits, 0)?;
        let ai = VkMemoryAllocateInfo {
            s_type: ST_MEMORY_ALLOCATE_INFO,
            p_next: std::ptr::null(),
            allocation_size: reqs.size,
            memory_type_index: memory_type,
        };
        let mut image_memory: u64 = 0;
        if unsafe { alloc_mem(self.device, &ai, std::ptr::null(), &mut image_memory) } != VK_SUCCESS
        {
            anyhow::bail!("vkAllocateMemory (image) failed");
        }
        if unsafe { bind_image(self.device, image, image_memory, 0) } != VK_SUCCESS {
            anyhow::bail!("vkBindImageMemory failed");
        }
        let ivci = VkImageViewCreateInfo {
            s_type: ST_IMAGE_VIEW_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            image,
            view_type: IMAGE_VIEW_TYPE_2D,
            format: FORMAT_R8_UNORM,
            components: VkComponentMapping {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            },
            subresource_range: VkImageSubresourceRange {
                aspect_mask: ASPECT_COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            },
        };
        let mut view: u64 = 0;
        if unsafe { create_view(self.device, &ivci, std::ptr::null(), &mut view) } != VK_SUCCESS {
            anyhow::bail!("vkCreateImageView (atlas) failed");
        }
        let sci = VkSamplerCreateInfo {
            s_type: ST_SAMPLER_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            mag_filter: FILTER_LINEAR,
            min_filter: FILTER_LINEAR,
            mipmap_mode: SAMPLER_MIPMAP_MODE_NEAREST,
            address_mode_u: SAMPLER_ADDRESS_CLAMP_TO_EDGE,
            address_mode_v: SAMPLER_ADDRESS_CLAMP_TO_EDGE,
            address_mode_w: SAMPLER_ADDRESS_CLAMP_TO_EDGE,
            mip_lod_bias: 0.0,
            anisotropy_enable: 0,
            max_anisotropy: 1.0,
            compare_enable: 0,
            compare_op: 0,
            min_lod: 0.0,
            max_lod: 0.0,
            border_color: BORDER_COLOR_FLOAT_OPAQUE_WHITE,
            unnormalized_coordinates: 0,
        };
        let mut sampler: u64 = 0;
        if unsafe { create_sampler(self.device, &sci, std::ptr::null(), &mut sampler) }
            != VK_SUCCESS
        {
            anyhow::bail!("vkCreateSampler failed");
        }

        let binding = VkDescriptorSetLayoutBinding {
            binding: 0,
            descriptor_type: DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
            descriptor_count: 1,
            stage_flags: SHADER_STAGE_FRAGMENT,
            p_immutable_samplers: std::ptr::null(),
        };
        let dslci = VkDescriptorSetLayoutCreateInfo {
            s_type: ST_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            binding_count: 1,
            p_bindings: &binding,
        };
        let mut descriptor_set_layout: u64 = 0;
        if unsafe {
            create_set_layout(
                self.device,
                &dslci,
                std::ptr::null(),
                &mut descriptor_set_layout,
            )
        } != VK_SUCCESS
        {
            anyhow::bail!("vkCreateDescriptorSetLayout failed");
        }
        let pool_size = VkDescriptorPoolSize {
            ty: DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
            descriptor_count: 1,
        };
        let dpci = VkDescriptorPoolCreateInfo {
            s_type: ST_DESCRIPTOR_POOL_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            max_sets: 1,
            pool_size_count: 1,
            p_pool_sizes: &pool_size,
        };
        let mut descriptor_pool: u64 = 0;
        if unsafe { create_pool(self.device, &dpci, std::ptr::null(), &mut descriptor_pool) }
            != VK_SUCCESS
        {
            anyhow::bail!("vkCreateDescriptorPool failed");
        }
        let dsai = VkDescriptorSetAllocateInfo {
            s_type: ST_DESCRIPTOR_SET_ALLOCATE_INFO,
            p_next: std::ptr::null(),
            descriptor_pool,
            descriptor_set_count: 1,
            p_set_layouts: &descriptor_set_layout,
        };
        let mut descriptor_set: u64 = 0;
        if unsafe { alloc_sets(self.device, &dsai, &mut descriptor_set) } != VK_SUCCESS {
            anyhow::bail!("vkAllocateDescriptorSets failed");
        }
        let image_info = VkDescriptorImageInfo {
            sampler,
            image_view: view,
            image_layout: LAYOUT_SHADER_READ_ONLY_OPTIMAL,
        };
        let write = VkWriteDescriptorSet {
            s_type: ST_WRITE_DESCRIPTOR_SET,
            p_next: std::ptr::null(),
            dst_set: descriptor_set,
            dst_binding: 0,
            dst_array_element: 0,
            descriptor_count: 1,
            descriptor_type: DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
            p_image_info: &image_info,
            p_buffer_info: std::ptr::null(),
            p_texel_buffer_view: std::ptr::null(),
        };
        unsafe { update_sets(self.device, 1, &write, 0, std::ptr::null()) };

        let plci = VkPipelineLayoutCreateInfo {
            s_type: ST_PIPELINE_LAYOUT_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            set_layout_count: 1,
            p_set_layouts: &descriptor_set_layout,
            push_constant_range_count: 0,
            p_push_constant_ranges: std::ptr::null(),
        };
        let mut pipeline_layout: u64 = 0;
        if unsafe { create_layout(self.device, &plci, std::ptr::null(), &mut pipeline_layout) }
            != VK_SUCCESS
        {
            anyhow::bail!("vkCreatePipelineLayout failed");
        }

        let spirv = load_spirv();
        let smci = VkShaderModuleCreateInfo {
            s_type: ST_SHADER_MODULE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            code_size: spirv.len() * 4,
            p_code: spirv.as_ptr(),
        };
        let mut shader: u64 = 0;
        if unsafe { create_shader(self.device, &smci, std::ptr::null(), &mut shader) } != VK_SUCCESS
        {
            anyhow::bail!("vkCreateShaderModule failed");
        }
        let entry = CString::new("vs_main").unwrap();
        let frag_entry = CString::new("fs_main").unwrap();
        let stages = [
            VkPipelineShaderStageCreateInfo {
                s_type: ST_PIPELINE_SHADER_STAGE_CREATE_INFO,
                p_next: std::ptr::null(),
                flags: 0,
                stage: SHADER_STAGE_VERTEX,
                module: shader,
                p_name: entry.as_ptr(),
                p_specialization_info: std::ptr::null(),
            },
            VkPipelineShaderStageCreateInfo {
                s_type: ST_PIPELINE_SHADER_STAGE_CREATE_INFO,
                p_next: std::ptr::null(),
                flags: 0,
                stage: SHADER_STAGE_FRAGMENT,
                module: shader,
                p_name: frag_entry.as_ptr(),
                p_specialization_info: std::ptr::null(),
            },
        ];
        let vertex_binding = VkVertexInputBindingDescription {
            binding: 0,
            stride: 32,
            input_rate: VERTEX_INPUT_RATE_VERTEX,
        };
        let attributes = [
            VkVertexInputAttributeDescription {
                location: 0,
                binding: 0,
                format: FORMAT_R32G32_SFLOAT,
                offset: 0,
            },
            VkVertexInputAttributeDescription {
                location: 1,
                binding: 0,
                format: FORMAT_R32G32_SFLOAT,
                offset: 8,
            },
            VkVertexInputAttributeDescription {
                location: 2,
                binding: 0,
                format: FORMAT_R32G32B32A32_SFLOAT,
                offset: 16,
            },
        ];
        let visci = VkPipelineVertexInputStateCreateInfo {
            s_type: ST_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            vertex_binding_description_count: 1,
            p_vertex_binding_descriptions: &vertex_binding,
            vertex_attribute_description_count: 3,
            p_vertex_attribute_descriptions: attributes.as_ptr(),
        };
        let iasci = VkPipelineInputAssemblyStateCreateInfo {
            s_type: ST_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            topology: TOPOLOGY_TRIANGLE_LIST,
            primitive_restart_enable: 0,
        };
        let viewport = VkViewport {
            x: 0.0,
            y: 0.0,
            width: self.width as f32,
            height: self.height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        };
        let scissor = VkRect2D {
            offset: [0, 0],
            extent: VkExtent2D {
                width: self.width,
                height: self.height,
            },
        };
        let vpsci = VkPipelineViewportStateCreateInfo {
            s_type: ST_PIPELINE_VIEWPORT_STATE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            viewport_count: 1,
            p_viewports: &viewport,
            scissor_count: 1,
            p_scissors: &scissor,
        };
        let rsci = VkPipelineRasterizationStateCreateInfo {
            s_type: ST_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            depth_clamp_enable: 0,
            rasterizer_discard_enable: 0,
            polygon_mode: POLYGON_MODE_FILL,
            cull_mode: CULL_MODE_NONE,
            front_face: FRONT_FACE_COUNTER_CLOCKWISE,
            depth_bias_enable: 0,
            depth_bias_constant_factor: 0.0,
            depth_bias_clamp: 0.0,
            depth_bias_slope_factor: 0.0,
            line_width: 1.0,
        };
        let msci = VkPipelineMultisampleStateCreateInfo {
            s_type: ST_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            rasterization_samples: SAMPLE_COUNT_1,
            sample_shading_enable: 0,
            min_sample_shading: 1.0,
            p_sample_mask: std::ptr::null(),
            alpha_to_coverage_enable: 0,
            alpha_to_one_enable: 0,
        };
        let blend_attachment = VkPipelineColorBlendAttachmentState {
            blend_enable: 1,
            src_color_blend_factor: BLEND_FACTOR_SRC_ALPHA,
            dst_color_blend_factor: BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
            color_blend_op: BLEND_OP_ADD,
            src_alpha_blend_factor: BLEND_FACTOR_SRC_ALPHA,
            dst_alpha_blend_factor: BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
            alpha_blend_op: BLEND_OP_ADD,
            color_write_mask: COLOR_COMPONENT_RGBA,
        };
        let cbsci = VkPipelineColorBlendStateCreateInfo {
            s_type: ST_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            logic_op_enable: 0,
            logic_op: 0,
            attachment_count: 1,
            p_attachments: &blend_attachment,
            blend_constants: [0.0; 4],
        };
        let dynamic_states = [DYNAMIC_STATE_VIEWPORT, DYNAMIC_STATE_SCISSOR];
        let dsci = VkPipelineDynamicStateCreateInfo {
            s_type: ST_PIPELINE_DYNAMIC_STATE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            dynamic_state_count: 2,
            p_dynamic_states: dynamic_states.as_ptr(),
        };
        let gpci = VkGraphicsPipelineCreateInfo {
            s_type: ST_GRAPHICS_PIPELINE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            stage_count: 2,
            p_stages: stages.as_ptr(),
            p_vertex_input_state: &visci,
            p_input_assembly_state: &iasci,
            p_tessellation_state: std::ptr::null(),
            p_viewport_state: &vpsci,
            p_rasterization_state: &rsci,
            p_multisample_state: &msci,
            p_depth_stencil_state: std::ptr::null(),
            p_color_blend_state: &cbsci,
            p_dynamic_state: &dsci,
            layout: pipeline_layout,
            render_pass: self.render_pass,
            subpass: 0,
            base_pipeline_handle: 0,
            base_pipeline_index: -1,
        };
        let mut pipeline: u64 = 0;
        if unsafe { create_pipelines(self.device, 0, 1, &gpci, std::ptr::null(), &mut pipeline) }
            != VK_SUCCESS
        {
            anyhow::bail!("vkCreateGraphicsPipelines failed");
        }

        let vertex_size: u64 = 2 * 1024 * 1024;
        let (vertex_buffer, vertex_memory, vertex_mapped) =
            self.create_host_buffer(vertex_size, BUFFER_USAGE_VERTEX_BUFFER)?;

        self.text = Some(TextPipeline {
            descriptor_set_layout,
            descriptor_pool,
            descriptor_set,
            pipeline_layout,
            pipeline,
            image,
            image_memory,
            view,
            sampler,
            vertex_buffer,
            vertex_memory,
            vertex_mapped,
            vertex_size,
            atlas_extent: (ATLAS, ATLAS),
        });
        Ok(())
    }

    /// Create the rounded-rectangle pipeline: no descriptors, only a
    /// host-visible vertex buffer, with alpha blending enabled.
    fn create_quad_pipeline(&mut self) -> anyhow::Result<()> {
        let create_layout: PFN_vkCreatePipelineLayout =
            dev_fn!(self, "vkCreatePipelineLayout", PFN_vkCreatePipelineLayout);
        let create_shader: PFN_vkCreateShaderModule =
            dev_fn!(self, "vkCreateShaderModule", PFN_vkCreateShaderModule);
        let create_pipelines: PFN_vkCreateGraphicsPipelines = dev_fn!(
            self,
            "vkCreateGraphicsPipelines",
            PFN_vkCreateGraphicsPipelines
        );

        let plci = VkPipelineLayoutCreateInfo {
            s_type: ST_PIPELINE_LAYOUT_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            set_layout_count: 0,
            p_set_layouts: std::ptr::null(),
            push_constant_range_count: 0,
            p_push_constant_ranges: std::ptr::null(),
        };
        let mut pipeline_layout: u64 = 0;
        if unsafe { create_layout(self.device, &plci, std::ptr::null(), &mut pipeline_layout) }
            != VK_SUCCESS
        {
            anyhow::bail!("vkCreatePipelineLayout (quad) failed");
        }

        let spirv = load_quad_spirv();
        let smci = VkShaderModuleCreateInfo {
            s_type: ST_SHADER_MODULE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            code_size: spirv.len() * 4,
            p_code: spirv.as_ptr(),
        };
        let mut shader: u64 = 0;
        if unsafe { create_shader(self.device, &smci, std::ptr::null(), &mut shader) } != VK_SUCCESS
        {
            anyhow::bail!("vkCreateShaderModule (quad) failed");
        }
        let entry = CString::new("vs_main").unwrap();
        let frag_entry = CString::new("fs_main").unwrap();
        let stages = [
            VkPipelineShaderStageCreateInfo {
                s_type: ST_PIPELINE_SHADER_STAGE_CREATE_INFO,
                p_next: std::ptr::null(),
                flags: 0,
                stage: SHADER_STAGE_VERTEX,
                module: shader,
                p_name: entry.as_ptr(),
                p_specialization_info: std::ptr::null(),
            },
            VkPipelineShaderStageCreateInfo {
                s_type: ST_PIPELINE_SHADER_STAGE_CREATE_INFO,
                p_next: std::ptr::null(),
                flags: 0,
                stage: SHADER_STAGE_FRAGMENT,
                module: shader,
                p_name: frag_entry.as_ptr(),
                p_specialization_info: std::ptr::null(),
            },
        ];
        let vertex_binding = VkVertexInputBindingDescription {
            binding: 0,
            stride: std::mem::size_of::<QuadVertex>() as u32,
            input_rate: VERTEX_INPUT_RATE_VERTEX,
        };
        let attributes = [
            (0u32, FORMAT_R32G32_SFLOAT, 0u32),
            (1, FORMAT_R32G32_SFLOAT, 8),
            (2, FORMAT_R32G32_SFLOAT, 16),
            (3, FORMAT_R32G32B32A32_SFLOAT, 24),
            (4, FORMAT_R32G32_SFLOAT, 40),
            (5, FORMAT_R32G32B32A32_SFLOAT, 48),
            (6, FORMAT_R32G32B32A32_SFLOAT, 64),
        ]
        .map(
            |(location, format, offset)| VkVertexInputAttributeDescription {
                location,
                binding: 0,
                format,
                offset,
            },
        );
        let visci = VkPipelineVertexInputStateCreateInfo {
            s_type: ST_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            vertex_binding_description_count: 1,
            p_vertex_binding_descriptions: &vertex_binding,
            vertex_attribute_description_count: attributes.len() as u32,
            p_vertex_attribute_descriptions: attributes.as_ptr(),
        };
        let iasci = VkPipelineInputAssemblyStateCreateInfo {
            s_type: ST_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            topology: TOPOLOGY_TRIANGLE_LIST,
            primitive_restart_enable: 0,
        };
        let viewport = VkViewport {
            x: 0.0,
            y: 0.0,
            width: self.width as f32,
            height: self.height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        };
        let scissor = VkRect2D {
            offset: [0, 0],
            extent: VkExtent2D {
                width: self.width,
                height: self.height,
            },
        };
        let vpsci = VkPipelineViewportStateCreateInfo {
            s_type: ST_PIPELINE_VIEWPORT_STATE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            viewport_count: 1,
            p_viewports: &viewport,
            scissor_count: 1,
            p_scissors: &scissor,
        };
        let rsci = VkPipelineRasterizationStateCreateInfo {
            s_type: ST_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            depth_clamp_enable: 0,
            rasterizer_discard_enable: 0,
            polygon_mode: POLYGON_MODE_FILL,
            cull_mode: CULL_MODE_NONE,
            front_face: FRONT_FACE_COUNTER_CLOCKWISE,
            depth_bias_enable: 0,
            depth_bias_constant_factor: 0.0,
            depth_bias_clamp: 0.0,
            depth_bias_slope_factor: 0.0,
            line_width: 1.0,
        };
        let msci = VkPipelineMultisampleStateCreateInfo {
            s_type: ST_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            rasterization_samples: SAMPLE_COUNT_1,
            sample_shading_enable: 0,
            min_sample_shading: 1.0,
            p_sample_mask: std::ptr::null(),
            alpha_to_coverage_enable: 0,
            alpha_to_one_enable: 0,
        };
        let blend_attachment = VkPipelineColorBlendAttachmentState {
            blend_enable: 1,
            src_color_blend_factor: BLEND_FACTOR_SRC_ALPHA,
            dst_color_blend_factor: BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
            color_blend_op: BLEND_OP_ADD,
            src_alpha_blend_factor: BLEND_FACTOR_SRC_ALPHA,
            dst_alpha_blend_factor: BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
            alpha_blend_op: BLEND_OP_ADD,
            color_write_mask: COLOR_COMPONENT_RGBA,
        };
        let cbsci = VkPipelineColorBlendStateCreateInfo {
            s_type: ST_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            logic_op_enable: 0,
            logic_op: 0,
            attachment_count: 1,
            p_attachments: &blend_attachment,
            blend_constants: [0.0; 4],
        };
        let dynamic_states = [DYNAMIC_STATE_VIEWPORT, DYNAMIC_STATE_SCISSOR];
        let dsci = VkPipelineDynamicStateCreateInfo {
            s_type: ST_PIPELINE_DYNAMIC_STATE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            dynamic_state_count: 2,
            p_dynamic_states: dynamic_states.as_ptr(),
        };
        let gpci = VkGraphicsPipelineCreateInfo {
            s_type: ST_GRAPHICS_PIPELINE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            stage_count: 2,
            p_stages: stages.as_ptr(),
            p_vertex_input_state: &visci,
            p_input_assembly_state: &iasci,
            p_tessellation_state: std::ptr::null(),
            p_viewport_state: &vpsci,
            p_rasterization_state: &rsci,
            p_multisample_state: &msci,
            p_depth_stencil_state: std::ptr::null(),
            p_color_blend_state: &cbsci,
            p_dynamic_state: &dsci,
            layout: pipeline_layout,
            render_pass: self.render_pass,
            subpass: 0,
            base_pipeline_handle: 0,
            base_pipeline_index: -1,
        };
        let mut pipeline: u64 = 0;
        if unsafe { create_pipelines(self.device, 0, 1, &gpci, std::ptr::null(), &mut pipeline) }
            != VK_SUCCESS
        {
            anyhow::bail!("vkCreateGraphicsPipelines (quad) failed");
        }

        let vertex_size: u64 = 4 * 1024 * 1024;
        let (vertex_buffer, vertex_memory, vertex_mapped) =
            self.create_host_buffer(vertex_size, BUFFER_USAGE_VERTEX_BUFFER)?;
        self.quads = Some(QuadPipeline {
            pipeline_layout,
            pipeline,
            vertex_buffer,
            vertex_memory,
            vertex_mapped,
            vertex_size,
        });
        Ok(())
    }

    fn create_host_buffer(&self, size: u64, usage: u32) -> anyhow::Result<(u64, u64, *mut u8)> {
        let create_buffer: PFN_vkCreateBuffer =
            inst_fn!(self, "vkCreateBuffer", PFN_vkCreateBuffer);
        let get_reqs: PFN_vkGetBufferMemoryRequirements = inst_fn!(
            self,
            "vkGetBufferMemoryRequirements",
            PFN_vkGetBufferMemoryRequirements
        );
        let alloc_mem: PFN_vkAllocateMemory =
            inst_fn!(self, "vkAllocateMemory", PFN_vkAllocateMemory);
        let bind_mem: PFN_vkBindBufferMemory =
            inst_fn!(self, "vkBindBufferMemory", PFN_vkBindBufferMemory);
        let map_mem: PFN_vkMapMemory = inst_fn!(self, "vkMapMemory", PFN_vkMapMemory);
        let bci = VkBufferCreateInfo {
            s_type: ST_BUFFER_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            size,
            usage,
            sharing_mode: SHARING_MODE_EXCLUSIVE,
            queue_family_index_count: 0,
            p_queue_family_indices: std::ptr::null(),
        };
        let mut buffer: u64 = 0;
        if unsafe { create_buffer(self.device, &bci, std::ptr::null(), &mut buffer) } != VK_SUCCESS
        {
            anyhow::bail!("vkCreateBuffer failed");
        }
        let mut reqs = VkMemoryRequirements {
            size: 0,
            alignment: 0,
            memory_type_bits: 0,
        };
        unsafe { get_reqs(self.device, buffer, &mut reqs) };
        let memory_type = self.find_memory_type(
            reqs.memory_type_bits,
            MEMORY_PROPERTY_HOST_VISIBLE | MEMORY_PROPERTY_HOST_COHERENT,
        )?;
        let ai = VkMemoryAllocateInfo {
            s_type: ST_MEMORY_ALLOCATE_INFO,
            p_next: std::ptr::null(),
            allocation_size: reqs.size,
            memory_type_index: memory_type,
        };
        let mut memory: u64 = 0;
        if unsafe { alloc_mem(self.device, &ai, std::ptr::null(), &mut memory) } != VK_SUCCESS {
            anyhow::bail!("vkAllocateMemory failed");
        }
        if unsafe { bind_mem(self.device, buffer, memory, 0) } != VK_SUCCESS {
            anyhow::bail!("vkBindBufferMemory failed");
        }
        let mut mapped: *mut c_void = std::ptr::null_mut();
        if unsafe { map_mem(self.device, memory, 0, VK_WHOLE_SIZE, 0, &mut mapped) } != VK_SUCCESS {
            anyhow::bail!("vkMapMemory failed");
        }
        Ok((buffer, memory, mapped as *mut u8))
    }

    pub fn render_scene(
        &mut self,
        color: [f32; 4],
        quads: &[QuadVertex],
        vertices: &[GlyphVertex],
        uploads: &[super::atlas::AtlasUpload],
    ) -> anyhow::Result<()> {
        if self.text.is_none() {
            self.create_text_pipeline()?;
        }
        if self.quads.is_none() {
            self.create_quad_pipeline()?;
        }
        let copy_image: PFN_vkCmdCopyBufferToImage =
            dev_fn!(self, "vkCmdCopyBufferToImage", PFN_vkCmdCopyBufferToImage);
        let pipeline_barrier: PFN_vkCmdPipelineBarrier =
            dev_fn!(self, "vkCmdPipelineBarrier", PFN_vkCmdPipelineBarrier);
        unsafe {
            (self.fns.wait_for_fences)(self.device, 1, &self.in_flight, 1, u64::MAX);
            (self.fns.reset_fences)(self.device, 1, &self.in_flight);
        }
        let mut image_index: u32 = 0;
        let ar = unsafe {
            (self.fns.acquire_next_image)(
                self.device,
                self.swapchain,
                u64::MAX,
                self.image_available,
                0,
                &mut image_index,
            )
        };
        if ar == VK_ERROR_OUT_OF_DATE_KHR {
            let (w, h) = (self.width, self.height);
            self.resize(w, h)?;
            return Ok(());
        }
        if ar != VK_SUCCESS {
            anyhow::bail!("vkAcquireNextImageKHR failed: {ar}");
        }
        {
            let quad_pipeline = self.quads.as_ref().expect("quad pipeline");
            let quad_bytes = std::mem::size_of_val(quads);
            if quad_bytes as u64 > quad_pipeline.vertex_size {
                anyhow::bail!("quad vertex buffer too small");
            }
            if !quads.is_empty() {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        quads.as_ptr() as *const u8,
                        quad_pipeline.vertex_mapped,
                        quad_bytes,
                    );
                }
            }
        }
        let vertex_bytes = std::mem::size_of_val(vertices);
        {
            let text = self.text.as_ref().expect("text pipeline");
            if vertex_bytes as u64 > text.vertex_size {
                anyhow::bail!("glyph vertex buffer too small");
            }
            if !vertices.is_empty() {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        vertices.as_ptr() as *const u8,
                        text.vertex_mapped,
                        vertex_bytes,
                    );
                }
            }
        }
        unsafe { (self.fns.reset_command_buffer)(self.command_buffer, 0) };
        let cbbi = VkCommandBufferBeginInfo {
            s_type: ST_COMMAND_BUFFER_BEGIN_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            p_inheritance_info: std::ptr::null(),
        };
        if unsafe { (self.fns.begin_command_buffer)(self.command_buffer, &cbbi) } != VK_SUCCESS {
            anyhow::bail!("vkBeginCommandBuffer failed");
        }
        if !uploads.is_empty() {
            let total: u64 = uploads.iter().map(|u| (u.width * u.height) as u64).sum();
            let (staging_buffer, staging_memory, staging_mapped) =
                self.create_host_buffer(total.max(4), BUFFER_USAGE_TRANSFER_SRC)?;
            let mut offset: u64 = 0;
            let mut regions = Vec::with_capacity(uploads.len());
            for upload in uploads {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        upload.data.as_ptr(),
                        staging_mapped.add(offset as usize),
                        upload.data.len(),
                    );
                }
                regions.push(VkBufferImageCopy {
                    buffer_offset: offset,
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: VkImageSubresourceLayers {
                        aspect_mask: ASPECT_COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    },
                    image_offset: VkOffset3D {
                        x: upload.x as i32,
                        y: upload.y as i32,
                        z: 0,
                    },
                    image_extent: VkExtent3D {
                        width: upload.width,
                        height: upload.height,
                        depth: 1,
                    },
                });
                offset += upload.data.len() as u64;
            }
            let text = self.text.as_ref().expect("text pipeline");
            let range = VkImageSubresourceRange {
                aspect_mask: ASPECT_COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            };
            let to_dst = VkImageMemoryBarrier {
                s_type: ST_IMAGE_MEMORY_BARRIER,
                p_next: std::ptr::null(),
                src_access_mask: ACCESS_SHADER_READ,
                dst_access_mask: ACCESS_TRANSFER_WRITE,
                old_layout: LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                new_layout: LAYOUT_TRANSFER_DST_OPTIMAL,
                src_queue_family_index: QUEUE_FAMILY_IGNORED,
                dst_queue_family_index: QUEUE_FAMILY_IGNORED,
                image: text.image,
                subresource_range: range,
            };
            let to_read = VkImageMemoryBarrier {
                s_type: ST_IMAGE_MEMORY_BARRIER,
                p_next: std::ptr::null(),
                src_access_mask: ACCESS_TRANSFER_WRITE,
                dst_access_mask: ACCESS_SHADER_READ,
                old_layout: LAYOUT_TRANSFER_DST_OPTIMAL,
                new_layout: LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                src_queue_family_index: QUEUE_FAMILY_IGNORED,
                dst_queue_family_index: QUEUE_FAMILY_IGNORED,
                image: text.image,
                subresource_range: range,
            };
            unsafe {
                pipeline_barrier(
                    self.command_buffer,
                    PIPELINE_STAGE_FRAGMENT_SHADER,
                    PIPELINE_STAGE_TRANSFER,
                    0,
                    0,
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    1,
                    &to_dst,
                );
                copy_image(
                    self.command_buffer,
                    staging_buffer,
                    text.image,
                    LAYOUT_TRANSFER_DST_OPTIMAL,
                    regions.len() as u32,
                    regions.as_ptr(),
                );
                pipeline_barrier(
                    self.command_buffer,
                    PIPELINE_STAGE_TRANSFER,
                    PIPELINE_STAGE_FRAGMENT_SHADER,
                    0,
                    0,
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    1,
                    &to_read,
                );
            }
            let _ = (staging_memory, staging_buffer);
        }
        let clear = VkClearValue {
            clear_color: VkClearColorValue { f: color },
        };
        let area = VkRect2D {
            offset: [0, 0],
            extent: VkExtent2D {
                width: self.width,
                height: self.height,
            },
        };
        let rpbi = VkRenderPassBeginInfo {
            s_type: ST_RENDER_PASS_BEGIN_INFO,
            p_next: std::ptr::null(),
            render_pass: self.render_pass,
            framebuffer: self.framebuffers[image_index as usize],
            render_area: area,
            clear_value_count: 1,
            p_clear_values: &clear,
        };
        unsafe {
            (self.fns.begin_render_pass)(self.command_buffer, &rpbi, SUBPASS_CONTENTS_INLINE)
        };
        if !quads.is_empty() {
            let bind_pipeline: PFN_vkCmdBindPipeline =
                dev_fn!(self, "vkCmdBindPipeline", PFN_vkCmdBindPipeline);
            let bind_vertex: PFN_vkCmdBindVertexBuffers =
                dev_fn!(self, "vkCmdBindVertexBuffers", PFN_vkCmdBindVertexBuffers);
            let draw: PFN_vkCmdDraw = dev_fn!(self, "vkCmdDraw", PFN_vkCmdDraw);
            let set_viewport: PFN_vkCmdSetViewport =
                dev_fn!(self, "vkCmdSetViewport", PFN_vkCmdSetViewport);
            let set_scissor: PFN_vkCmdSetScissor =
                dev_fn!(self, "vkCmdSetScissor", PFN_vkCmdSetScissor);
            let quad_pipeline = self.quads.as_ref().expect("quad pipeline");
            let viewport = VkViewport {
                x: 0.0,
                y: 0.0,
                width: self.width as f32,
                height: self.height as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            };
            let scissor = VkRect2D {
                offset: [0, 0],
                extent: VkExtent2D {
                    width: self.width,
                    height: self.height,
                },
            };
            unsafe {
                set_viewport(self.command_buffer, 0, 1, &viewport);
                set_scissor(self.command_buffer, 0, 1, &scissor);
                bind_pipeline(
                    self.command_buffer,
                    BIND_POINT_GRAPHICS,
                    quad_pipeline.pipeline,
                );
                bind_vertex(
                    self.command_buffer,
                    0,
                    1,
                    &quad_pipeline.vertex_buffer,
                    &0u64,
                );
                draw(self.command_buffer, quads.len() as u32, 1, 0, 0);
            }
        }
        if !vertices.is_empty() {
            let bind_pipeline: PFN_vkCmdBindPipeline =
                dev_fn!(self, "vkCmdBindPipeline", PFN_vkCmdBindPipeline);
            let bind_vertex: PFN_vkCmdBindVertexBuffers =
                dev_fn!(self, "vkCmdBindVertexBuffers", PFN_vkCmdBindVertexBuffers);
            let bind_sets: PFN_vkCmdBindDescriptorSets =
                dev_fn!(self, "vkCmdBindDescriptorSets", PFN_vkCmdBindDescriptorSets);
            let draw: PFN_vkCmdDraw = dev_fn!(self, "vkCmdDraw", PFN_vkCmdDraw);
            let set_viewport: PFN_vkCmdSetViewport =
                dev_fn!(self, "vkCmdSetViewport", PFN_vkCmdSetViewport);
            let set_scissor: PFN_vkCmdSetScissor =
                dev_fn!(self, "vkCmdSetScissor", PFN_vkCmdSetScissor);
            let text = self.text.as_ref().expect("text pipeline");
            let viewport = VkViewport {
                x: 0.0,
                y: 0.0,
                width: self.width as f32,
                height: self.height as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            };
            let scissor = VkRect2D {
                offset: [0, 0],
                extent: VkExtent2D {
                    width: self.width,
                    height: self.height,
                },
            };
            unsafe {
                set_viewport(self.command_buffer, 0, 1, &viewport);
                set_scissor(self.command_buffer, 0, 1, &scissor);
                bind_pipeline(self.command_buffer, BIND_POINT_GRAPHICS, text.pipeline);
                bind_vertex(self.command_buffer, 0, 1, &text.vertex_buffer, &0u64);
                bind_sets(
                    self.command_buffer,
                    BIND_POINT_GRAPHICS,
                    text.pipeline_layout,
                    0,
                    1,
                    &text.descriptor_set,
                    0,
                    std::ptr::null(),
                );
                draw(self.command_buffer, vertices.len() as u32, 1, 0, 0);
            }
        }
        unsafe {
            (self.fns.cmd_end_render_pass)(self.command_buffer);
            (self.fns.end_command_buffer)(self.command_buffer);
        }
        let wait_stage = [PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT];
        let cmds: [*const c_void; 1] = [self.command_buffer];
        let si = VkSubmitInfo {
            s_type: ST_SUBMIT_INFO,
            p_next: std::ptr::null(),
            wait_semaphore_count: 1,
            p_wait_semaphores: &self.image_available,
            p_wait_dst_stage_mask: wait_stage.as_ptr(),
            command_buffer_count: 1,
            p_command_buffers: cmds.as_ptr(),
            signal_semaphore_count: 1,
            p_signal_semaphores: &self.render_finished,
        };
        if unsafe { (self.fns.queue_submit)(self.queue, 1, &si, self.in_flight) } != VK_SUCCESS {
            anyhow::bail!("vkQueueSubmit failed");
        }
        unsafe { (self.fns.wait_for_fences)(self.device, 1, &self.in_flight, 1, u64::MAX) };
        let pi = VkPresentInfoKHR {
            s_type: ST_PRESENT_INFO_KHR,
            p_next: std::ptr::null(),
            wait_semaphore_count: 1,
            p_wait_semaphores: &self.render_finished,
            swapchain_count: 1,
            p_swapchains: &self.swapchain,
            p_image_indices: &image_index,
            p_results: std::ptr::null_mut(),
        };
        let pr = unsafe { (self.fns.queue_present)(self.queue, &pi) };
        if pr == VK_ERROR_OUT_OF_DATE_KHR {
            let (w, h) = (self.width, self.height);
            self.resize(w, h)?;
            return Ok(());
        }
        if pr != VK_SUCCESS {
            anyhow::bail!("vkQueuePresentKHR failed: {pr}");
        }
        Ok(())
    }
}
