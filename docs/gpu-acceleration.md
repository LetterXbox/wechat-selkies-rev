# GPU Acceleration Guide

WeChat Selkies Rev uses **Selkies 2.2+** (`pixelflux` + XLibre `Xvfb` with Glamor/DRI3) to keep desktop rendering and H.264/VP9 video encoding on the GPU whenever a compatible render node is passed into the container.

By default, the image configures:

```env
SELKIES_ENCODER=h264enc,vp9enc,h265enc,vp8enc
SELKIES_RATE_CONTROL_MODE=crf,cbr
SELKIES_VIDEO_CRF=25
DISABLE_DRI3=false
```

`jpeg` is intentionally excluded from the default encoder list so streams do not silently fall back to CPU-heavy JPEG striping.

## 1. Intel iGPU / Arc (VA-API / QuickSync)

Works on bare-metal Linux hosts and Proxmox VMs with an Intel iGPU passthrough or SR-IOV VF (for example, Intel Alder Lake-N with `i915-sriov-dkms`).

### Host Prerequisites

1. Verify a DRM render node exists on the Docker host:
   ```bash
   ls -l /dev/dri
   ```
   You should see `/dev/dri/renderD128` (or another `renderD*` node if multiple GPUs exist).
2. On Proxmox guests using an Intel SR-IOV VF (`i915-sriov-dkms`), confirm the driver bound cleanly:
   ```bash
   lspci -nnk | grep -A3 -E 'VGA|Display'
   sudo dmesg | grep -iE 'i915|SR-IOV|GuC'
   ```
3. Pass `/dev/dri` into the container. The base image's `init-video` service automatically matches the container user's group to the host render node's GID, so no manual `--group-add` is required.

### Compose Configuration

Already enabled by default in `docker-compose.yml`:

```yaml
services:
  wechat-selkies-rev:
    devices:
      - /dev/dri:/dev/dri
    environment:
      - DRINODE=/dev/dri/renderD128
      - DRI_NODE=/dev/dri/renderD128
      - DISABLE_DRI3=false
```

- `DRINODE` selects the render node for `Xvfb -glamor -dri` (OpenGL/DRI3 rendering).
- `DRI_NODE` selects the render node for VA-API video encoding.
- Gen11+ Intel GPUs automatically use `intel-media-va-driver-non-free` (`iHD`); older Gen8–Gen10 GPUs automatically fall back to `i965-va-driver-shaders` (`i965`).

## 2. AMD Radeon / Ryzen iGPU (VA-API / Mesa)

Use the same `/dev/dri` passthrough as Intel (`mesa-va-drivers` and `mesa-vulkan-drivers` are included in the base image):

```yaml
services:
  wechat-selkies-rev:
    devices:
      - /dev/dri:/dev/dri
    environment:
      - DRINODE=/dev/dri/renderD128
      - DRI_NODE=/dev/dri/renderD128
      - DISABLE_DRI3=false
```

If the host has both an Intel iGPU and a discrete AMD GPU, check which `renderD*` maps to `amdgpu` before setting `DRINODE` and `DRI_NODE`:

```bash
for n in /dev/dri/renderD*; do
  echo "$n -> $(basename "$(readlink -f "/sys/class/drm/$(basename "$n")/device/driver")")"
done
```

## 3. NVIDIA GPU (NVENC + EGL/DRI3)

Requires the [NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html) on the Docker host.

Add a Compose override (or edit `docker-compose.yml`):

```yaml
services:
  wechat-selkies-rev:
    environment:
      - NVIDIA_VISIBLE_DEVICES=all
      - NVIDIA_DRIVER_CAPABILITIES=all
      - DISABLE_DRI3=false
    deploy:
      resources:
        reservations:
          devices:
            - driver: nvidia
              count: 1
              capabilities: [gpu]
```

Remove Intel/AMD-specific `DRINODE` / `DRI_NODE` overrides unless you explicitly want to pin a specific `/dev/dri/renderD*` node alongside NVIDIA.

## 4. Software Fallback (No GPU or Troubleshooting)

WeChat Selkies Rev runs cleanly without a GPU using x264/libvpx software encoding:

1. Remove the `devices: ["/dev/dri:/dev/dri"]` section and unset `DRINODE` and `DRI_NODE` in `.env` / `docker-compose.yml`.
2. **Troubleshooting a flaky virtual GPU / SR-IOV VF:**
   - If the container restarts in a loop or shows no desktop under a virtual GPU, set `DISABLE_DRI3=true` (switches `Xvfb` to software rendering while keeping VA-API encoding on `DRI_NODE`).
   - If the container is healthy (`wechat-supervisor healthcheck` passes) but the browser stream stays black, unset `DRI_NODE` to switch video encoding to software while keeping Glamor or software rendering.

## 5. Verifying Hardware Acceleration

1. **Check Xvfb DRI3 startup in container logs:**
   ```bash
   docker logs wechat-selkies-rev 2>&1 | grep -iE 'DRI3|glamor|i965|NVIDIA|va-api'
   ```
   Expected line on Intel:
   ```text
   **** Xvfb using DRI3 on /dev/dri/renderD128 (i915) ****
   ```
2. **Verify VA-API profiles inside the container (Intel/AMD):**
   ```bash
   docker exec wechat-selkies-rev vainfo --display drm --device /dev/dri/renderD128
   ```
   Look for `VAEntrypointEncSlice` / `VAEntrypointEncSliceLP` entries under `VAProfileH264Main` / `VAProfileH264High` or `VAProfileVP9Profile0`.
3. **Confirm in the Selkies browser sidebar:**
   Open the Selkies side menu -> **Stats** while scrolling WeChat. Verify the active encoder is `h264enc` or `vp9enc` (hardware VA-API/NVENC) with low encode latency and client-side WebCodecs decoding.
