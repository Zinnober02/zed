#include <stddef.h>
#include <stdint.h>
int snd_card_next(void) { return (int)-1; }
int snd_ctl_card_info(void) { return (int)-1; }
void snd_ctl_card_info_free(void) {  }
const void * snd_ctl_card_info_get_name(void) { return 0; }
int snd_ctl_card_info_malloc(void) { return (int)-1; }
int snd_ctl_close(void) { return (int)-1; }
int snd_ctl_open(void) { return (int)-1; }
int snd_ctl_pcm_info(void) { return (int)-1; }
int snd_ctl_pcm_next_device(void) { return (int)-1; }
int snd_device_name_free_hint(void) { return (int)-1; }
void * snd_device_name_get_hint(void) { return 0; }
int snd_device_name_hint(void) { return (int)-1; }
long snd_pcm_avail(void) { return -1; }
long snd_pcm_bytes_to_frames(void) { return -1; }
int snd_pcm_close(void) { return (int)-1; }
int snd_pcm_get_params(void) { return (int)-1; }
int snd_pcm_hw_params(void) { return (int)-1; }
int snd_pcm_hw_params_any(void) { return (int)-1; }
int snd_pcm_hw_params_can_pause(void) { return (int)-1; }
void snd_pcm_hw_params_free(void) {  }
int snd_pcm_hw_params_get_buffer_size_max(void) { return (int)-1; }
int snd_pcm_hw_params_get_buffer_size_min(void) { return (int)-1; }
int snd_pcm_hw_params_get_channels_max(void) { return (int)-1; }
int snd_pcm_hw_params_get_channels_min(void) { return (int)-1; }
int snd_pcm_hw_params_get_period_size(void) { return (int)-1; }
int snd_pcm_hw_params_get_rate_max(void) { return (int)-1; }
int snd_pcm_hw_params_get_rate_min(void) { return (int)-1; }
int snd_pcm_hw_params_malloc(void) { return (int)-1; }
int snd_pcm_hw_params_set_access(void) { return (int)-1; }
int snd_pcm_hw_params_set_buffer_size_near(void) { return (int)-1; }
int snd_pcm_hw_params_set_channels(void) { return (int)-1; }
int snd_pcm_hw_params_set_format(void) { return (int)-1; }
int snd_pcm_hw_params_set_period_size_near(void) { return (int)-1; }
int snd_pcm_hw_params_set_rate(void) { return (int)-1; }
int snd_pcm_hw_params_test_channels(void) { return (int)-1; }
int snd_pcm_hw_params_test_format(void) { return (int)-1; }
int snd_pcm_hw_params_test_rate(void) { return (int)-1; }
void snd_pcm_info_free(void) {  }
const void * snd_pcm_info_get_name(void) { return 0; }
int snd_pcm_info_malloc(void) { return (int)-1; }
void snd_pcm_info_set_device(void) {  }
void snd_pcm_info_set_stream(void) {  }
void snd_pcm_info_set_subdevice(void) {  }
int snd_pcm_open(void) { return (int)-1; }
int snd_pcm_pause(void) { return (int)-1; }
int snd_pcm_poll_descriptors(void) { return (int)-1; }
int snd_pcm_poll_descriptors_count(void) { return (int)-1; }
int snd_pcm_poll_descriptors_revents(void) { return (int)-1; }
int snd_pcm_prepare(void) { return (int)-1; }
long snd_pcm_readi(void) { return -1; }
int snd_pcm_recover(void) { return (int)-1; }
int snd_pcm_start(void) { return (int)-1; }
int snd_pcm_status(void) { return (int)-1; }
long snd_pcm_status_get_delay(void) { return -1; }
void snd_pcm_status_get_htstamp(void) {  }
void snd_pcm_status_get_trigger_htstamp(void) {  }
long snd_pcm_status_sizeof(void) { return -1; }
int snd_pcm_sw_params(void) { return (int)-1; }
int snd_pcm_sw_params_current(void) { return (int)-1; }
void snd_pcm_sw_params_free(void) {  }
int snd_pcm_sw_params_malloc(void) { return (int)-1; }
int snd_pcm_sw_params_set_avail_min(void) { return (int)-1; }
int snd_pcm_sw_params_set_start_threshold(void) { return (int)-1; }
int snd_pcm_sw_params_set_tstamp_mode(void) { return (int)-1; }
int snd_pcm_sw_params_set_tstamp_type(void) { return (int)-1; }
long snd_pcm_writei(void) { return -1; }

/* ---- OHOS musl gaps ---------------------------------------------------- */
/* musl has no robust mutexes; rustix references these but the startup path
   never calls them. ENOTSUP = 95. */
int pthread_mutexattr_setrobust(void) { return 95; }
int pthread_mutex_consistent(void) { return 95; }

/* Rust allocator/crypto/zstd integration hooks: weak elsewhere, unused here. */
void *OPENSSL_memory_alloc(void) { return 0; }
void OPENSSL_memory_free(void) { }
size_t OPENSSL_memory_get_size(void) { return 0; }
void *OPENSSL_memory_realloc(void) { return 0; }
void ZSTD_trace_compress_begin(void) { }
void ZSTD_trace_compress_end(void) { }
void ZSTD_trace_decompress_begin(void) { }
void ZSTD_trace_decompress_end(void) { }
void sdallocx(void) { }

/* libgcc unwinder entry points OHOS ships without; the cdylib registers its
   frame info through these. */
void __register_frame_info(void *begin, void *object) { (void)begin; (void)object; }
void __deregister_frame_info(void *begin) { (void)begin; }
void __at_fini(void) { }

