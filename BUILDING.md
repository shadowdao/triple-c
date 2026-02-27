# Building Triple-C

Triple-C is a Tauri v2 desktop application with a React/TypeScript frontend and a Rust backend. This guide covers building the app from source on Linux and Windows.

## Prerequisites (All Platforms)

- **Node.js 22** LTS — https://nodejs.org/
- **Rust** (stable) — https://rustup.rs/
- **Git**

## Linux

### 1. Install system dependencies

Ubuntu / Debian:

```bash
sudo apt-get update
sudo apt-get install -y \
  libgtk-3-dev \
  libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  libsoup-3.0-dev \
  patchelf \
  libssl-dev \
  pkg-config \
  build-essential
```

Fedora:

```bash
sudo dnf install -y \
  gtk3-devel \
  webkit2gtk4.1-devel \
  libayatana-appindicator-gtk3-devel \
  librsvg2-devel \
  libsoup3-devel \
  patchelf \
  openssl-devel \
  pkg-config \
  gcc
```

Arch:

```bash
sudo pacman -S --needed \
  gtk3 \
  webkit2gtk-4.1 \
  libayatana-appindicator \
  librsvg \
  libsoup3 \
  patchelf \
  openssl \
  pkg-config \
  base-devel
```

### 2. Install frontend dependencies

```bash
cd app
npm ci
```

### 3. Build

```bash
npx tauri build
```

Build artifacts are located in `app/src-tauri/target/release/bundle/`:

| Format     | Path                          |
|------------|-------------------------------|
| AppImage   | `appimage/*.AppImage`         |
| Debian pkg | `deb/*.deb`                   |
| RPM pkg    | `rpm/*.rpm`                   |

## Windows

### 1. Install prerequisites

- **Visual Studio Build Tools** or **Visual Studio** with the "Desktop development with C++" workload — https://visualstudio.microsoft.com/visual-cpp-build-tools/
- **WebView2** — pre-installed on Windows 10 (1803+) and Windows 11. If missing, download the Evergreen Bootstrapper from https://developer.microsoft.com/en-us/microsoft-edge/webview2/

### 2. Install frontend dependencies

```powershell
cd app
npm ci
```

### 3. Build

```powershell
npx tauri build
```

Build artifacts are located in `app\src-tauri\target\release\bundle\`:

| Format | Path              |
|--------|-------------------|
| MSI    | `msi\*.msi`       |
| NSIS   | `nsis\*.exe`      |

## Development Mode

To run the app in development mode with hot-reload:

```bash
cd app
npm ci          # if not already done
npx tauri dev
```

## Container Image

The sandbox container image (used at runtime by the app) is built automatically by CI when files under `container/` change. To build it locally:

```bash
docker build -t triple-c-sandbox ./container
```
