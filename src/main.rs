use windows::{
    Win32::{
        Foundation::*,
        Graphics::{
            Direct3D::Fxc::*, Direct3D::*, Direct3D11::*, Dxgi::Common::*, Dxgi::*, Gdi::*,
            Imaging::*,
        },
        System::Com::*,
        System::LibraryLoader::*,
        UI::HiDpi::*,
        UI::Input::KeyboardAndMouse::*,
        UI::WindowsAndMessaging::*,
    },
    core::*,
};

struct PixelShaderConfig {
    name: String,
    compiled: ID3D11PixelShader,
}

struct CaptureState {
    start_time: std::time::Instant,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    swap_chain: IDXGISwapChain1,
    dxgi_adapter: IDXGIAdapter,
    duplication: Option<IDXGIOutputDuplication>,
    vertex_shader: ID3D11VertexShader,
    pixel_shaders: Vec<PixelShaderConfig>,
    current_shader: usize,
    compute_shader: ID3D11ComputeShader,
    extend_params_buffer: ID3D11Buffer,
    sampler: ID3D11SamplerState,
    vertex_buffer: ID3D11Buffer,
    render_target_view: Option<ID3D11RenderTargetView>,
    shader_resource_view: Option<ID3D11ShaderResourceView>,
    input_layout: ID3D11InputLayout,
    time_buffer: ID3D11Buffer,

    staging_texture: Option<ID3D11Texture2D>,
    extended_texture: Option<ID3D11Texture2D>,
    extended_srv: Option<ID3D11ShaderResourceView>,
    extended_uav: Option<ID3D11UnorderedAccessView>,
    source_rect: RECT,

    always_on_top: bool,
    paused: bool,
    hwnd: HWND,
}

#[repr(C)]
struct Vertex {
    position: [f32; 2],
    tex_coord: [f32; 2],
}

const VERTEX_SHADER: &[u8] = b"
struct VS_INPUT {
    float2 pos : POSITION;
    float2 tex : TEXCOORD;
};

struct VS_OUTPUT {
    float4 pos : SV_POSITION;
    float2 tex : TEXCOORD;
};

VS_OUTPUT main(VS_INPUT input) {
    VS_OUTPUT output;
    output.pos = float4(input.pos, 0.0f, 1.0f);
    output.tex = input.tex;
    return output;
}";

#[repr(C)]
struct ExtendParams {
    src_size: [u32; 2],
    dst_size: [u32; 2],
    src_offset: [i32; 2],
    padding: [u32; 2],
}

const EXTEND_COMPUTE_SHADER: &[u8] = b"
Texture2D<float4> srcTexture : register(t0);
RWTexture2D<float4> dstTexture : register(u0);

cbuffer ExtendParams : register(b0) {
    uint2 srcSize;
    uint2 dstSize;
    int2 srcOffset;  // Where the source starts in the destination
    uint2 padding;
}

[numthreads(8, 8, 1)]
void main(uint3 dispatchThreadID : SV_DispatchThreadID) {
    uint2 dstPos = dispatchThreadID.xy;

    if (dstPos.x >= dstSize.x || dstPos.y >= dstSize.y)
        return;

    // Calculate source position (may be out of bounds)
    int2 srcPos = int2(dstPos) - srcOffset;

    // Clamp to source texture bounds (sample and hold)
    srcPos.x = clamp(srcPos.x, 0, (int)srcSize.x - 1);
    srcPos.y = clamp(srcPos.y, 0, (int)srcSize.y - 1);

    // Read from source and write to destination
    float4 color = srcTexture.Load(int3(srcPos, 0));
    dstTexture[dstPos] = color;
}";

const PIXEL_SHADER_PASSTHRU: &[u8] = include_bytes!("../shaders/passthru.hlsl");
const PIXEL_SHADER_WOBBLY: &[u8] = include_bytes!("../shaders/wobbly.hlsl");
const PIXEL_SHADER_LIGHTNING: &[u8] = include_bytes!("../shaders/lightning.hlsl");
const PIXEL_SHADER_SORTY: &[u8] = include_bytes!("../shaders/sorty.hlsl");

fn main() -> Result<()> {
    unsafe {
        // Enable DPI awareness for proper scaling
        // Ignore errors if DPI awareness is already set
        _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
    }

    let window_class = w!("ScreenCaptureClass");
    let hinstance: HINSTANCE = unsafe { GetModuleHandleW(None) }?.into();

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        lpszClassName: window_class,
        ..Default::default()
    };

    unsafe {
        RegisterClassExW(&wc);
    }

    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            window_class,
            w!("Screen Capture"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1280,
            720,
            None,
            None,
            Some(hinstance),
            None,
        )?
    };
    println!("created window");

    unsafe {
        SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)?;
    }

    #[cfg(debug_assertions)]
    let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_DEBUG;
    #[cfg(not(debug_assertions))]
    let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;

    // Create D3D11 device and context
    let (device, context) = unsafe {
        let mut device = None;
        let mut context = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            flags,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
        (device.unwrap(), context.unwrap())
    };

    // Create swap chain
    let dxgi_device: IDXGIDevice = device.cast()?;
    let dxgi_adapter: IDXGIAdapter = unsafe { dxgi_device.GetAdapter()? };
    let dxgi_factory: IDXGIFactory2 = unsafe { dxgi_adapter.GetParent()? };

    let mut client_rect = RECT::default();
    unsafe { GetClientRect(hwnd, &mut client_rect)? };

    let swap_chain_desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: (client_rect.right - client_rect.left) as u32,
        Height: (client_rect.bottom - client_rect.top) as u32,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        Stereo: BOOL::from(false),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        AlphaMode: DXGI_ALPHA_MODE_UNSPECIFIED,
        Flags: 0,
    };

    let swap_chain = unsafe {
        dxgi_factory.CreateSwapChainForHwnd(&device, hwnd, &swap_chain_desc, None, None)?
    };
    println!("created swapchain");

    // Create shaders
    let (vertex_shader, input_layout) = unsafe {
        let (shader_blob, error_blob, res) = d3d_compile(
            VERTEX_SHADER,
            PCSTR::null(),                                   // source name (optional)
            None,                                            // defines (optional)
            None,                                            // include handler (optional)
            s!("main"),                                      // entry point
            s!("vs_4_0"),                                    // target profile
            D3DCOMPILE_DEBUG | D3DCOMPILE_SKIP_OPTIMIZATION, // compilation flags
            0,                                               // secondary flags
        );
        println!("vertex shader compilation complete");

        if let Some(error) = error_blob {
            let error_message =
                std::str::from_utf8(blob_as_slice(&error)).unwrap_or("Unknown error");
            println!("Shader compilation error: {}", error_message);
        }

        res?;

        let Some(blob) = shader_blob else {
            return Err(Error::new(E_FAIL, "Failed to compile vertex shader"));
        };
        let shader_byte_code = blob_as_slice(&blob);
        let shader = {
            let mut shader_out = None;
            device.CreateVertexShader(shader_byte_code, None, Some(&mut shader_out))?;
            shader_out.ok_or(E_POINTER)?
        };

        let input_elements = [
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: s!("POSITION"),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R32G32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: 0,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: s!("TEXCOORD"),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R32G32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: D3D11_APPEND_ALIGNED_ELEMENT,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
        ];

        let input_layout = {
            let mut layout_out = None;
            device.CreateInputLayout(&input_elements, shader_byte_code, Some(&mut layout_out))?;
            layout_out.ok_or(E_POINTER)?
        };

        (shader, input_layout)
    };
    println!("created vertex shader");

    // Helper closure to compile pixel shaders
    let compile_pixel_shader = |shader_source: &[u8], name: &str| -> Result<ID3D11PixelShader> {
        unsafe {
            let (shader_blob, error_blob, res) = d3d_compile(
                shader_source,
                None,                                            // source name (optional)
                None,                                            // defines (optional)
                None,                                            // include handler (optional)
                s!("main"),                                      // entry point
                s!("ps_4_0"),                                    // target profile
                D3DCOMPILE_DEBUG | D3DCOMPILE_SKIP_OPTIMIZATION, // compilation flags
                0,                                               // secondary flags
            );

            if let Some(error) = error_blob {
                let error_message =
                    std::str::from_utf8(blob_as_slice(&error)).unwrap_or("Unknown error");
                println!("{} shader compilation error: {}", name, error_message);
            }

            res?;

            let Some(blob) = shader_blob else {
                return Err(Error::new(
                    E_FAIL,
                    format!("Failed to compile {} pixel shader", name),
                ));
            };

            let mut shader_out = None;
            device.CreatePixelShader(blob_as_slice(&blob), None, Some(&mut shader_out))?;
            shader_out.ok_or_else(|| E_POINTER.into())
        }
    };

    let shader_inputs = vec![
        ("passthru", PIXEL_SHADER_PASSTHRU),
        ("wobbly", PIXEL_SHADER_WOBBLY),
        ("lightning", PIXEL_SHADER_LIGHTNING),
        ("sorty", PIXEL_SHADER_SORTY),
    ];
    let pixel_shaders = shader_inputs
        .into_iter()
        .map(|v| PixelShaderConfig {
            name: v.0.to_string(),
            compiled: compile_pixel_shader(v.1, v.0).unwrap(),
        })
        .collect::<Vec<_>>();
    println!("compiled pixel shaders");

    // Create compute shader for texture extension
    let compute_shader = unsafe {
        let (shader_blob, error_blob, res) = d3d_compile(
            EXTEND_COMPUTE_SHADER,
            None,                                            // source name (optional)
            None,                                            // defines (optional)
            None,                                            // include handler (optional)
            s!("main"),                                      // entry point
            s!("cs_5_0"),                                    // target profile
            D3DCOMPILE_DEBUG | D3DCOMPILE_SKIP_OPTIMIZATION, // compilation flags
            0,                                               // secondary flags
        );
        println!("compute shader compilation complete {:?}", res);

        if let Some(error) = error_blob {
            let error_message =
                std::str::from_utf8(blob_as_slice(&error)).unwrap_or("Unknown error");
            println!("Compute shader compilation error: {}", error_message);
        }

        res?;

        let Some(blob) = shader_blob else {
            return Err(Error::new(E_FAIL, "Failed to compile compute shader"));
        };

        let mut shader_out = None;
        device.CreateComputeShader(blob_as_slice(&blob), None, Some(&mut shader_out))?;
        shader_out.ok_or(E_POINTER)?
    };
    println!("created compute shader");

    // Create extend params buffer
    let extend_params_buffer_desc = D3D11_BUFFER_DESC {
        ByteWidth: std::mem::size_of::<ExtendParams>() as u32,
        Usage: D3D11_USAGE_DYNAMIC,
        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
        MiscFlags: 0,
        StructureByteStride: 0,
    };

    let extend_params_buffer = unsafe {
        let mut buffer_out = None;
        device.CreateBuffer(&extend_params_buffer_desc, None, Some(&mut buffer_out))?;
        buffer_out.ok_or(E_POINTER)?
    };

    // Create sampler state
    let sampler_desc = D3D11_SAMPLER_DESC {
        Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
        AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
        MipLODBias: 0.0,
        MaxAnisotropy: 1,
        ComparisonFunc: D3D11_COMPARISON_NEVER,
        BorderColor: [0.0; 4],
        MinLOD: 0.0,
        MaxLOD: D3D11_FLOAT32_MAX,
    };

    let sampler = unsafe {
        let mut sampler_out = None;
        device.CreateSamplerState(&sampler_desc, Some(&mut sampler_out))?;
        sampler_out.ok_or(E_POINTER)?
    };
    println!("created sampler");

    // Create vertex buffer with fullscreen quad
    let vertices = [
        Vertex {
            position: [-1.0, -1.0],
            tex_coord: [0.0, 1.0],
        },
        Vertex {
            position: [-1.0, 1.0],
            tex_coord: [0.0, 0.0],
        },
        Vertex {
            position: [1.0, -1.0],
            tex_coord: [1.0, 1.0],
        },
        Vertex {
            position: [1.0, 1.0],
            tex_coord: [1.0, 0.0],
        },
    ];

    let vertex_buffer_desc = D3D11_BUFFER_DESC {
        ByteWidth: std::mem::size_of_val(&vertices) as u32,
        Usage: D3D11_USAGE_IMMUTABLE,
        BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
        StructureByteStride: 0,
    };

    let vertex_data = D3D11_SUBRESOURCE_DATA {
        pSysMem: vertices.as_ptr() as *const _,
        SysMemPitch: 0,
        SysMemSlicePitch: 0,
    };

    let vertex_buffer = unsafe {
        let mut buffer_out = None;
        device.CreateBuffer(
            &vertex_buffer_desc,
            Some(&vertex_data),
            Some(&mut buffer_out),
        )?;
        buffer_out.ok_or(E_POINTER)?
    };

    let time_buffer_desc = D3D11_BUFFER_DESC {
        ByteWidth: 16,
        Usage: D3D11_USAGE_DYNAMIC,
        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
        MiscFlags: 0,
        StructureByteStride: 0,
    };

    let time_buffer = unsafe {
        let mut buffer_out = None;
        device.CreateBuffer(&time_buffer_desc, None, Some(&mut buffer_out))?;
        buffer_out.ok_or(E_POINTER)?
    };

    let capture_state = CaptureState {
        start_time: std::time::Instant::now(),
        device,
        context,
        swap_chain,
        dxgi_adapter,
        duplication: None,
        vertex_shader,
        pixel_shaders,
        current_shader: 1,
        compute_shader,
        extend_params_buffer,
        sampler,
        vertex_buffer,
        render_target_view: None,
        shader_resource_view: None,
        input_layout,
        time_buffer,
        staging_texture: None,
        extended_texture: None,
        extended_srv: None,
        extended_uav: None,
        source_rect: RECT::default(),
        always_on_top: false,
        paused: false,
        hwnd,
    };
    println!("created capture state");
    println!(
        "Current shader: {} (press 1 - {} to switch)",
        capture_state.pixel_shaders[capture_state.current_shader].name,
        capture_state.pixel_shaders.len(),
    );

    unsafe {
        SetWindowLongPtrW(
            hwnd,
            GWLP_USERDATA,
            Box::into_raw(Box::new(capture_state)) as isize,
        );

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);
    }

    let mut message = MSG::default();
    loop {
        unsafe {
            let status = GetMessageW(&mut message, None, 0, 0);
            if status.0 == 0 {
                break;
            }
            if status.0 == -1 {
                println!("GetMessageW failed with -1");
                break;
            }
            _ = TranslateMessage(&message);
            _ = DispatchMessageW(&message);
        }
    }

    unsafe {
        let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut CaptureState;
        if !state_ptr.is_null() {
            drop(Box::from_raw(state_ptr));
        }
    }

    Ok(())
}

extern "system" fn wndproc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match message {
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_SIZE | WM_MOVE => {
                let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut CaptureState;
                if !state_ptr.is_null() {
                    let state = &mut *state_ptr;
                    // Update screen position
                    let mut client_origin = POINT::default();
                    let _ = ClientToScreen(hwnd, &mut client_origin);
                    let mut client_rect = RECT::default();
                    let _ = GetClientRect(hwnd, &mut client_rect);
                    let mut source_rect = client_rect;
                    source_rect.left += client_origin.x;
                    source_rect.right += client_origin.x;
                    source_rect.top += client_origin.y;
                    source_rect.bottom += client_origin.y;
                    state.source_rect = source_rect;

                    if message == WM_SIZE {
                        state.render_target_view = None;
                        state.staging_texture = None; // Recreate on size change
                        state.extended_texture = None; // Recreate on size change
                        state.extended_srv = None;
                        state.extended_uav = None;
                        if let Err(_) = resize_swapchain(state, hwnd) {
                            // Handle error if needed
                        }
                    }
                }
                LRESULT(0)
            }
            WM_SETCURSOR => {
                // If the cursor is in the client area, set it to the arrow
                if (lparam.0 as u32 & 0xFFFF) == HTCLIENT {
                    SetCursor(LoadCursorW(None, IDC_ARROW).ok());
                    LRESULT(1) // TRUE - we handled it
                } else {
                    // Let Windows handle non-client areas (borders, title bar, etc.)
                    DefWindowProcW(hwnd, message, wparam, lparam)
                }
            }
            WM_PAINT => {
                let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut CaptureState;
                if !state_ptr.is_null() {
                    let state = &mut *state_ptr;
                    if !state.paused {
                        if let Err(e) = capture_and_render_frame(state, hwnd) {
                            // Handle error if needed
                            println!("error {:?}", e);
                            if e.code() == DXGI_ERROR_ACCESS_LOST {
                                state.duplication = None;
                            }
                        }
                    }
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut CaptureState;
                if !state_ptr.is_null() {
                    let state = &mut *state_ptr;
                    let vkey = wparam.0 as i32;

                    // Check if Ctrl is pressed
                    let ctrl_pressed = (GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0;

                    if ctrl_pressed {
                        match vkey {
                            0x53 => {
                                // 'S' key
                                if let Err(e) = save_frame_to_png(state) {
                                    println!("Failed to save frame: {:?}", e);
                                }
                            }
                            0x41 => {
                                // 'A' key
                                if let Err(e) = toggle_always_on_top(state) {
                                    println!("Failed to toggle always on top: {:?}", e);
                                }
                            }
                            _ => {}
                        }
                    } else {
                        match vkey {
                            0x31..=0x39 => {
                                // Number keys for shader switching (no Ctrl needed)
                                let idx = (vkey - 0x31) as usize;
                                if idx < state.pixel_shaders.len() {
                                    println!(
                                        "Switched to {} shader",
                                        state.pixel_shaders[idx].name
                                    );
                                    state.current_shader = idx
                                }
                            }
                            0x13 => {
                                // 'PAUSE' key
                                if let Err(e) = toggle_pause_and_hide(state) {
                                    println!("Failed to toggle pause and hide: {:?}", e);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

fn save_frame_to_png(state: &mut CaptureState) -> Result<()> {
    unsafe {
        // Get the back buffer from the swap chain (this has the shaded output)
        let back_buffer: ID3D11Texture2D = state.swap_chain.GetBuffer(0)?;

        // Get texture description
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        back_buffer.GetDesc(&mut desc);

        // Create a staging texture for CPU readback
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: desc.Width,
            Height: desc.Height,
            MipLevels: 1,
            ArraySize: 1,
            Format: desc.Format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };

        let mut staging_texture = None;
        state
            .device
            .CreateTexture2D(&staging_desc, None, Some(&mut staging_texture))?;
        let staging_texture = staging_texture.ok_or(E_POINTER)?;

        // Copy the back buffer to staging
        state.context.CopyResource(&staging_texture, &back_buffer);

        let width = desc.Width;
        let height = desc.Height;
        // Write pixels
        let (pixel_buffer, stride) = {
            let mut pixel_buffer = Vec::new();

            // Map the staging texture to read the pixels
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            state
                .context
                .Map(&staging_texture, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;

            let stride = mapped.RowPitch;
            let buffer_size = stride * height;
            pixel_buffer.extend_from_slice(std::slice::from_raw_parts(
                mapped.pData as *const u8,
                buffer_size as usize,
            ));

            // Unmap the texture
            state.context.Unmap(&staging_texture, 0);

            (pixel_buffer, stride)
        };

        // Generate timestamped filename
        let now = {
            let t = time::OffsetDateTime::now_utc();
            match time::UtcOffset::local_offset_at(t) {
                Ok(offset) => t.to_offset(offset),
                Err(_) => t,
            }
        };
        let format: &[time::format_description::FormatItem<'_>] = time::macros::format_description!(
            "[year]-[month]-[day]_[hour]_[minute]_[second]_[subsecond digits:3]"
        );
        let timestamp = now.format(format).unwrap();
        let filename = format!("scrimshady_{}.png", timestamp);

        let filename_wide: Vec<u16> = filename.encode_utf16().chain(std::iter::once(0)).collect();

        // Create WIC factory
        let wic_factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;

        // Create stream for file
        let stream = wic_factory.CreateStream()?;
        stream.InitializeFromFilename(PCWSTR(filename_wide.as_ptr()), GENERIC_WRITE.0)?;

        // Create PNG encoder
        let encoder = wic_factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;

        // Create frame
        let mut frame = None;
        encoder.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.ok_or(E_POINTER)?;
        frame.Initialize(None)?;
        frame.SetSize(width, height)?;

        // Set pixel format to BGRA (which matches our texture format)
        let mut pixel_format = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut pixel_format)?;

        // Write pixels
        frame.WritePixels(height, stride, &pixel_buffer)?;

        // Commit frame and encoder
        frame.Commit()?;
        encoder.Commit()?;

        println!("Screenshot saved: {}", filename);
    }
    Ok(())
}

fn toggle_always_on_top(state: &mut CaptureState) -> Result<()> {
    unsafe {
        state.always_on_top = !state.always_on_top;

        let hwnd_insert_after = if state.always_on_top {
            HWND_TOPMOST
        } else {
            HWND_NOTOPMOST
        };

        SetWindowPos(
            state.hwnd,
            Some(hwnd_insert_after),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE,
        )?;

        println!(
            "Always on top: {}",
            if state.always_on_top {
                "enabled"
            } else {
                "disabled"
            }
        );
    }
    Ok(())
}

fn toggle_pause_and_hide(state: &mut CaptureState) -> Result<()> {
    state.paused = !state.paused;

    let flags = if state.paused {
        WINDOW_DISPLAY_AFFINITY(0)
    } else {
        WDA_EXCLUDEFROMCAPTURE
    };
    unsafe { SetWindowDisplayAffinity(state.hwnd, flags) }?;

    println!(
        "Window: {}",
        if state.paused {
            "paused and capturable"
        } else {
            "rendering and excluded from capture"
        }
    );
    Ok(())
}

fn resize_swapchain(state: &mut CaptureState, hwnd: HWND) -> Result<()> {
    // Release old views
    state.render_target_view = None;
    state.shader_resource_view = None;

    unsafe {
        // Get new size
        let mut client_rect = RECT::default();
        GetClientRect(hwnd, &mut client_rect)?;
        let width = (client_rect.right - client_rect.left) as u32;
        let height = (client_rect.bottom - client_rect.top) as u32;

        // Resize the swap chain
        state.swap_chain.ResizeBuffers(
            2,
            width,
            height,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            DXGI_SWAP_CHAIN_FLAG(0),
        )?;

        // Recreate render target view
        let buffer: ID3D11Texture2D = state.swap_chain.GetBuffer(0)?;
        let mut render_target_view = None;
        state
            .device
            .CreateRenderTargetView(&buffer, None, Some(&mut render_target_view))?;
        state.render_target_view = render_target_view;
    }
    Ok(())
}

fn handle_frame(state: &mut CaptureState, frame_texture: IDXGIResource, hwnd: HWND) -> Result<()> {
    unsafe {
        // Get client area in screen coordinates
        let mut client_rect = RECT::default();
        GetClientRect(hwnd, &mut client_rect)?;
        let width = client_rect.right - client_rect.left;
        let height = client_rect.bottom - client_rect.top;

        // Get screen texture dimensions
        let texture: ID3D11Texture2D = frame_texture.cast()?;
        let mut screen_desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut screen_desc);

        // Calculate source box (may extend beyond screen bounds)
        let src_left = state.source_rect.left;
        let src_top = state.source_rect.top;
        let src_right = state.source_rect.left + width;
        let src_bottom = state.source_rect.top + height;

        // Calculate how much we extend beyond screen bounds
        let extend_left = (-src_left).max(0);
        let extend_top = (-src_top).max(0);
        let extend_right = (src_right - screen_desc.Width as i32).max(0);
        let extend_bottom = (src_bottom - screen_desc.Height as i32).max(0);

        // Calculate extended texture size
        let extended_width = (width + extend_left + extend_right) as u32;
        let extended_height = (height + extend_top + extend_bottom) as u32;

        // Create staging texture if needed (matches window size)
        if state.staging_texture.is_none() {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width as u32,
                Height: height as u32,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };

            let mut texture_out = None;
            state
                .device
                .CreateTexture2D(&desc, None, Some(&mut texture_out))?;
            state.staging_texture = texture_out;
        }

        // Create extended texture if needed
        if state.extended_texture.is_none() {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: extended_width,
                Height: extended_height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_UNORDERED_ACCESS.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };

            let mut texture_out = None;
            state
                .device
                .CreateTexture2D(&desc, None, Some(&mut texture_out))?;
            state.extended_texture = texture_out;

            // Create UAV for compute shader output
            let extended_tex = state.extended_texture.as_ref().unwrap();
            let uav_desc = D3D11_UNORDERED_ACCESS_VIEW_DESC {
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_UAV { MipSlice: 0 },
                },
            };

            let mut uav_out = None;
            state.device.CreateUnorderedAccessView(
                extended_tex,
                Some(&uav_desc),
                Some(&mut uav_out),
            )?;
            state.extended_uav = uav_out;

            // Create SRV for the extended texture
            let srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_SRV {
                        MostDetailedMip: 0,
                        MipLevels: 1,
                    },
                },
            };

            let mut srv_out = None;
            state.device.CreateShaderResourceView(
                extended_tex,
                Some(&srv_desc),
                Some(&mut srv_out),
            )?;
            state.extended_srv = srv_out;
        }

        // Clamp source box to valid screen coordinates
        let clamped_left = src_left.max(0).min(screen_desc.Width as i32);
        let clamped_top = src_top.max(0).min(screen_desc.Height as i32);
        let clamped_right = src_right.max(0).min(screen_desc.Width as i32);
        let clamped_bottom = src_bottom.max(0).min(screen_desc.Height as i32);

        // Copy the valid region to staging texture
        let dst_texture = state.staging_texture.as_ref().unwrap();

        if clamped_right > clamped_left && clamped_bottom > clamped_top {
            let src_box = D3D11_BOX {
                left: clamped_left as u32,
                top: clamped_top as u32,
                front: 0,
                right: clamped_right as u32,
                bottom: clamped_bottom as u32,
                back: 1,
            };

            // Destination offset should be zero - we're copying to a window-sized texture
            // The extension happens in the compute shader
            let dst_x = 0;
            let dst_y = 0;

            state.context.CopySubresourceRegion(
                dst_texture,
                0,
                dst_x,
                dst_y,
                0,
                &texture,
                0,
                Some(&src_box),
            );
        }

        // Create SRV for staging texture if needed
        if state.shader_resource_view.is_none() {
            let srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_SRV {
                        MostDetailedMip: 0,
                        MipLevels: 1,
                    },
                },
            };

            let mut srv_out = None;
            state.device.CreateShaderResourceView(
                dst_texture,
                Some(&srv_desc),
                Some(&mut srv_out),
            )?;
            state.shader_resource_view = srv_out;
        }

        // Run compute shader to extend the texture with edge padding
        {
            // Unbind pixel shader resources to avoid hazards
            state.context.PSSetShaderResources(0, Some(&[None]));

            let params = ExtendParams {
                src_size: [width as u32, height as u32],
                dst_size: [extended_width, extended_height],
                src_offset: [extend_left, extend_top],
                padding: [0, 0],
            };

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            state.context.Map(
                &state.extend_params_buffer,
                0,
                D3D11_MAP_WRITE_DISCARD,
                0,
                Some(&mut mapped),
            )?;
            std::ptr::copy_nonoverlapping(
                &params as *const ExtendParams as *const u8,
                mapped.pData as *mut u8,
                std::mem::size_of::<ExtendParams>(),
            );
            state.context.Unmap(&state.extend_params_buffer, 0);

            state.context.CSSetShader(&state.compute_shader, None);
            state
                .context
                .CSSetConstantBuffers(0, Some(&[Some(state.extend_params_buffer.clone())]));
            state.context.CSSetShaderResources(
                0,
                Some(&[Some(state.shader_resource_view.as_ref().unwrap().clone())]),
            );
            state.context.CSSetUnorderedAccessViews(
                0,
                1,
                Some(&Some(state.extended_uav.as_ref().unwrap().clone())),
                None,
            );

            let dispatch_x = extended_width.div_ceil(8);
            let dispatch_y = extended_height.div_ceil(8);
            state.context.Dispatch(dispatch_x, dispatch_y, 1);

            // Clear compute shader resources
            state.context.CSSetShader(None, None);
            state.context.CSSetShaderResources(0, Some(&[None]));
            state
                .context
                .CSSetUnorderedAccessViews(0, 1, Some(&None), None);
        }

        // update time buffer
        {
            let time = state.start_time.elapsed().as_secs_f32();

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            state.context.Map(
                &state.time_buffer,
                0,
                D3D11_MAP_WRITE_DISCARD,
                0,
                Some(&mut mapped),
            )?;
            *(mapped.pData as *mut f32) = time;
            state.context.Unmap(&state.time_buffer, 0);

            state
                .context
                .PSSetConstantBuffers(0, Some(&[Some(state.time_buffer.clone())]));
        }

        // Set up rendering pipeline
        let rtv = state.render_target_view.as_ref().unwrap();
        state
            .context
            .OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);

        {
            // Get current window size
            let mut client_rect = RECT::default();
            GetClientRect(hwnd, &mut client_rect)?;
            let width = (client_rect.right - client_rect.left) as f32;
            let height = (client_rect.bottom - client_rect.top) as f32;

            let viewport = D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: width,
                Height: height,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            state.context.RSSetViewports(Some(&[viewport]));
        };

        // Clear render target
        state
            .context
            .ClearRenderTargetView(rtv, &[0.0, 0.0, 0.0, 1.0]);

        // Set shaders and resources
        state.context.VSSetShader(&state.vertex_shader, None);
        let active_pixel_shader = &state.pixel_shaders[state.current_shader].compiled;
        state.context.PSSetShader(active_pixel_shader, None);
        state
            .context
            .PSSetSamplers(0, Some(&[Some(state.sampler.clone())]));
        // Use the extended texture instead of staging texture
        state.context.PSSetShaderResources(
            0,
            Some(&[Some(state.extended_srv.as_ref().unwrap().clone())]),
        );

        // Set vertex buffer
        let stride = std::mem::size_of::<Vertex>() as u32;
        let offset = 0;
        state.context.IASetVertexBuffers(
            0,
            1,
            Some(&Some(state.vertex_buffer.clone())),
            Some(&stride),
            Some(&offset),
        );
        state
            .context
            .IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);

        state.context.IASetInputLayout(&state.input_layout);

        // Draw
        state.context.Draw(4, 0);

        // Present
        state.swap_chain.Present(1, DXGI_PRESENT(0)).ok()?;

        //InvalidateRect(hwnd, None, false);
    }
    Ok(())
}

struct ReleaseFrameScope<'a>(Option<&'a IDXGIOutputDuplication>);

impl Drop for ReleaseFrameScope<'_> {
    fn drop(&mut self) {
        _ = self.try_drop()
    }
}

impl<'a> ReleaseFrameScope<'a> {
    fn try_drop(&mut self) -> Result<()> {
        if let Some(duplication) = self.0.take() {
            unsafe { duplication.ReleaseFrame() }?;
        }
        Ok(())
    }
    pub fn release(mut self) -> Result<()> {
        self.try_drop()
    }
    pub fn new(duplication: &'a IDXGIOutputDuplication) -> Self {
        Self(Some(duplication))
    }
}

struct AcquiredFrameScope<'a> {
    pub info: DXGI_OUTDUPL_FRAME_INFO,
    pub resource: Option<IDXGIResource>,
    release_scope: ReleaseFrameScope<'a>,
}

impl AcquiredFrameScope<'_> {
    fn release(self) -> Result<()> {
        self.release_scope.release()
    }
}

fn acquire_dxgi_duplication_frame<'a>(
    duplication: &'a IDXGIOutputDuplication,
    timeout_millis: u32,
) -> Result<AcquiredFrameScope<'a>> {
    let mut frame_resource = None;
    let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
    unsafe { duplication.AcquireNextFrame(timeout_millis, &mut frame_info, &mut frame_resource) }?;

    Ok(AcquiredFrameScope {
        info: frame_info,
        resource: frame_resource,
        release_scope: ReleaseFrameScope::new(duplication),
    })
}

fn capture_and_render_frame(state: &mut CaptureState, hwnd: HWND) -> Result<()> {
    unsafe {
        if state.duplication.is_none() {
            // Set up screen capture
            let output: IDXGIOutput = state.dxgi_adapter.EnumOutputs(0)?;
            let output1: IDXGIOutput1 = output.cast()?;
            state.duplication = Some(output1.DuplicateOutput(&state.device)?);
            println!("created dxgi duplication");
        }
        let duplication = state.duplication.clone().unwrap();

        match acquire_dxgi_duplication_frame(&duplication, 0) {
            Ok(frame) => {
                if frame.info.LastPresentTime != 0
                    && let Some(frame_texture) = frame.resource.clone()
                {
                    handle_frame(state, frame_texture, hwnd)?;
                }
                frame.release()?;
            }
            Err(e) => {
                if e.code() != DXGI_ERROR_WAIT_TIMEOUT {
                    return Err(e);
                }
            }
        };
    }
    Ok(())
}

unsafe fn d3d_compile<P0, P1, P2, P3>(
    sourcedata: &[u8],
    psourcename: P0,
    pdefines: Option<*const D3D_SHADER_MACRO>,
    pinclude: P1,
    pentrypoint: P2,
    ptarget: P3,
    flags1: u32,
    flags2: u32,
) -> (Option<ID3DBlob>, Option<ID3DBlob>, Result<()>)
where
    P0: windows::core::Param<PCSTR>,
    P1: windows::core::Param<ID3DInclude>,
    P2: windows::core::Param<PCSTR>,
    P3: windows::core::Param<PCSTR>,
{
    let mut shader_blob: Option<ID3DBlob> = None;
    let mut error_blob = None;
    let res = unsafe {
        D3DCompile(
            sourcedata.as_ptr() as *const _,
            sourcedata.len(),
            psourcename,
            pdefines,
            pinclude,
            pentrypoint,
            ptarget,
            flags1,
            flags2,
            &mut shader_blob,
            Some(&mut error_blob),
        )
    };
    (shader_blob, error_blob, res)
}

fn blob_as_slice(blob: &ID3DBlob) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize())
    }
}
