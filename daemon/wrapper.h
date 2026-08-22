/* Bindgen's single translation unit. VAAPI only -- `evdi_lib.h` was dropped
   along with the evdi capture path (see MILESTONES.md, Milestone 2: evdi froze
   this machine under both Wayland and X11, and capture moved to the ScreenCast
   portal / mutter, which never touch DRM/KMS). Keeping the include meant every
   build of this project needed libevdi-dev and the evdi DKMS kernel module for
   nothing at all. */
#include <va/va.h>
#include <va/va_drm.h>
#include <va/va_drmcommon.h>
#include <va/va_enc_h264.h>
#include <va/va_str.h>
#include <va/va_vpp.h>
