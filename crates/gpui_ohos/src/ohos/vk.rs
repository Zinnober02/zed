//! Hand-written, zero-dependency Vulkan renderer for OHOS.
//!
//! Everything goes through dlopen("libvulkan.so") plus vkGetInstanceProcAddr /
//! vkGetDeviceProcAddr; no ash, no wgpu, no build scripts. This is the same path
//! verified on the HarmonyOS PC (3120x2080 swapchain, clear + present).
//!
//! For now the renderer only clears the swapchain image. The GPUI scene pipeline
//! is layered on top of this in later milestones.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::atomic::{AtomicUsize, Ordering};

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
const VK_MAX_EXTENSION_NAME_SIZE: usize = 256;
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

type PFN_vkCmdClearAttachments = unsafe extern "C" fn(
    *mut c_void,
    u32,
    *const VkClearAttachment,
    u32,
    *const VkClearRect,
);

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
type PFN_vkGetSwapchainImagesKHR = unsafe extern "C" fn(*mut c_void, u64, *mut u32, *mut u64) -> i32;
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
type PFN_vkCreateSemaphore = unsafe extern "C" fn(
    *mut c_void,
    *const VkSemaphoreCreateInfo,
    *const c_void,
    *mut u64,
) -> i32;
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
}

impl VkRenderer {
    /// Create the whole Vulkan stack for an OHNativeWindow from an XComponent.
    pub fn new(window: *mut c_void, width: u32, height: u32) -> anyhow::Result<Self> {
        let libname = CString::new("libvulkan.so").unwrap();
        let lib = unsafe { dlopen(libname.as_ptr(), RTLD_NOW) };
        if lib.is_null() {
            anyhow::bail!("dlopen(libvulkan.so) failed");
        }

        let gpa_sym = unsafe { dlsym(lib, CString::new("vkGetInstanceProcAddr").unwrap().as_ptr()) };
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

        let cs_raw = unsafe { gpa(instance, CString::new("vkCreateSurfaceOHOS").unwrap().as_ptr()) };
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
        if unsafe { ephys(instance, &mut pcount, std::ptr::null_mut()) } != VK_SUCCESS || pcount == 0
        {
            anyhow::bail!("no physical device");
        }
        let mut phys = vec![0u64; pcount as usize];
        unsafe { ephys(instance, &mut pcount, phys.as_mut_ptr()) };
        let physical_device = phys[0];

        let qfp: PFN_vkGetPhysicalDeviceQueueFamilyProperties = unsafe {
            std::mem::transmute(gpa(
                instance,
                CString::new("vkGetPhysicalDeviceQueueFamilyProperties").unwrap().as_ptr(),
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
                CString::new("vkGetPhysicalDeviceSurfaceSupportKHR").unwrap().as_ptr(),
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
            std::mem::transmute(gpa(instance, CString::new("vkCreateDevice").unwrap().as_ptr()))
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
            create_swapchain: load!(gpd, device, "vkCreateSwapchainKHR", PFN_vkCreateSwapchainKHR),
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
            current_extent: VkExtent2D { width: 0, height: 0 },
            min_image_extent: VkExtent2D { width: 0, height: 0 },
            max_image_extent: VkExtent2D { width: 0, height: 0 },
            max_image_array_layers: 0,
            supported_transforms: 0,
            current_transform: 0,
            supported_composite_alpha: 0,
            supported_usage_flags: 0,
        };
        unsafe { caps_f(self.physical_device, self.surface, &mut caps) };

        let mut fcount: u32 = 0;
        unsafe { fmt_f(self.physical_device, self.surface, &mut fcount, std::ptr::null_mut()) };
        let mut fmts = vec![
            VkSurfaceFormatKHR {
                format: FMT_R8G8B8A8_UNORM,
                color_space: CS_SRGB_NONLINEAR,
            };
            fcount.max(1) as usize
        ];
        if fcount > 0 {
            unsafe { fmt_f(self.physical_device, self.surface, &mut fcount, fmts.as_mut_ptr()) };
        }
        let chosen = fmts
            .iter()
            .find(|f| f.format == FMT_R8G8B8A8_UNORM)
            .or_else(|| fmts.iter().find(|f| f.format == FMT_B8G8R8A8_SRGB))
            .copied()
            .unwrap_or(fmts[0]);
        self.format = chosen.format;

        let mut mcount: u32 = 0;
        unsafe { mode_f(self.physical_device, self.surface, &mut mcount, std::ptr::null_mut()) };
        let mut modes = vec![PRESENT_MODE_FIFO; mcount.max(1) as usize];
        if mcount > 0 {
            unsafe { mode_f(self.physical_device, self.surface, &mut mcount, modes.as_mut_ptr()) };
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
            let r =
                unsafe { (self.fns.create_framebuffer)(self.device, &fbci, std::ptr::null(), &mut fb) };
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

    /// Clear the swapchain image to color and present it.
    pub fn render_clear(&mut self, color: [f32; 4]) -> anyhow::Result<()> {
        self.render_frame(color, &[])
    }

    /// Clear the swapchain to color, then paint rects in order on top.
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
