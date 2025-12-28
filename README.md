# Scrim Shady

A lightweight Windows application that captures the screen region underneath its window and applies real-time HLSL pixel shaders to it using Direct3D 11.

## What It Does

Scrim Shady creates a window that acts as a "shader overlay" - it captures whatever is displayed on the screen beneath it and processes those pixels through customizable HLSL shaders before rendering them back. This allows you to apply visual effects like distortion, color grading, or custom filters to any part of your screen in real-time.

The app uses:
- Windows Desktop Duplication API (DXGI) for efficient screen capture
- SetWindowDisplayAffinity to exclude the app itself from the capture input
- Direct3D 11 for GPU-accelerated shader processing
- A compute shader to handle edge-padding when the window extends beyond screen boundaries

## Available Shaders

1. **passthru** - No effect, displays captured pixels as-is
2. **wobbly** - Wavy distortion effect
3. **lightning** - Lightning/electrical effect
4. **sorty** - Pixel sorting effect
5. **tiles** - Replace pixels with tiles from a sprite sheet

## Hotkeys

### Shader Selection
- **1-9** - Switch between different pixel shaders (listed above)

### Window Controls
- **Ctrl+A** - Toggle always-on-top mode for the window
- **Pause / Break** - Mark the window as capturable and pause rendering (useful for taking screenshots)

### Capture
- **Ctrl+S** - Save the current rendered frame as a PNG file with timestamp

## Demo

<img width="2004" height="1329" alt="Image" src="https://github.com/user-attachments/assets/08c90822-6811-476e-9426-95f529de5bcc" />

## Acknowldegments
Sprite sheet generated from [Cascadia Code](https://github.com/microsoft/cascadia-code)
