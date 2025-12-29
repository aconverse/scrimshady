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
        UI::Shell::*,
        UI::WindowsAndMessaging::*,
    },
    core::*,
};

enum ShaderType {
    Simple(ID3D11PixelShader),
    Tiles {
        shader: ID3D11PixelShader,
        spritesheet_srv: ID3D11ShaderResourceView,
        brightness_srv: ID3D11ShaderResourceView,
        constants_buffer: ID3D11Buffer,
        sheet_width: u32,
        sheet_height: u32,
        tiles_per_row: u32,
        total_tiles: usize,
    },
}

struct PixelShaderConfig {
    name: String,
    shader_type: ShaderType,
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
const PIXEL_SHADER_TILES: &[u8] = include_bytes!("../shaders/tiles.hlsl");
const FONT_SPRITESHEET_PNG: &[u8] = include_bytes!("../shaders/font_spritesheet.png");

#[repr(C)]
struct TilesConstants {
    source_resolution: [f32; 2],
    tile_size: [f32; 2],
    tiles_per_row: i32,
    total_tiles: i32,
    spritesheet_resolution: [f32; 2],
}

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

    // Helper closure to compile pixel shaders with shader model 5.0 (for StructuredBuffer support)
    let compile_pixel_shader_sm5 =
        |shader_source: &[u8], name: &str| -> Result<ID3D11PixelShader> {
            unsafe {
                let (shader_blob, error_blob, res) = d3d_compile(
                    shader_source,
                    None,                                            // source name (optional)
                    None,                                            // defines (optional)
                    None,                                            // include handler (optional)
                    s!("main"),                                      // entry point
                    s!("ps_5_0"),                                    // target profile (SM 5.0)
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
    let mut pixel_shaders = shader_inputs
        .into_iter()
        .map(|v| PixelShaderConfig {
            name: v.0.to_string(),
            shader_type: ShaderType::Simple(compile_pixel_shader(v.1, v.0).unwrap()),
        })
        .collect::<Vec<_>>();
    println!("compiled pixel shaders");

    // Compile and setup tiles shader (ASCII art effect)
    println!("Setting up tiles shader...");
    let tiles_shader = compile_pixel_shader_sm5(PIXEL_SHADER_TILES, "tiles")?;

    // Load the font spritesheet from embedded bytes
    let (_sheet_tex, sheet_srv, sheet_w, sheet_h, pixels) =
        load_png_from_bytes(&device, FONT_SPRITESHEET_PNG, "font_spritesheet.png")?;

    // Determine tile layout (8x16 character tiles)
    let tile_w = 8u32;
    let tile_h = 16u32;
    let tiles_per_row = sheet_w / tile_w;

    // Compute brightness for each tile
    let brightness = compute_tile_brightness(&pixels, sheet_w, sheet_h, tile_w, tile_h);

    // Create structured buffer for brightness values
    println!(
        "Creating structured buffer: {} elements, {} bytes",
        brightness.len(),
        brightness.len() * std::mem::size_of::<f32>()
    );
    let brightness_buffer = unsafe {
        let buffer_desc = D3D11_BUFFER_DESC {
            ByteWidth: (brightness.len() * std::mem::size_of::<f32>()) as u32,
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: D3D11_RESOURCE_MISC_BUFFER_STRUCTURED.0 as u32,
            StructureByteStride: std::mem::size_of::<f32>() as u32,
        };

        let buffer_data = D3D11_SUBRESOURCE_DATA {
            pSysMem: brightness.as_ptr() as *const _,
            SysMemPitch: 0,
            SysMemSlicePitch: 0,
        };

        let mut buffer_out = None;
        device.CreateBuffer(&buffer_desc, Some(&buffer_data), Some(&mut buffer_out))?;
        buffer_out.ok_or(E_POINTER)?
    };
    println!("Structured buffer created successfully");

    // Create SRV for structured buffer
    println!(
        "Creating SRV for structured buffer with {} elements",
        brightness.len()
    );
    let brightness_srv = unsafe {
        let mut srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
            Format: DXGI_FORMAT_UNKNOWN,
            ViewDimension: D3D11_SRV_DIMENSION_BUFFER,
            Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                Buffer: std::mem::zeroed(),
            },
        };

        // Set buffer parameters through the union
        srv_desc.Anonymous.Buffer.Anonymous1.FirstElement = 0;
        srv_desc.Anonymous.Buffer.Anonymous2.NumElements = brightness.len() as u32;

        let mut srv_out = None;
        let result = device.CreateShaderResourceView(
            &brightness_buffer,
            Some(&srv_desc),
            Some(&mut srv_out),
        );
        if let Err(e) = result {
            println!("ERROR creating SRV: {:?}", e);
            return Err(e);
        }
        srv_out.ok_or(E_POINTER)?
    };
    println!("SRV created successfully");

    // Create constant buffer for tiles shader parameters
    println!(
        "Creating constant buffer ({} bytes)",
        std::mem::size_of::<TilesConstants>()
    );
    let tiles_constants_buffer = unsafe {
        let buffer_desc = D3D11_BUFFER_DESC {
            ByteWidth: std::mem::size_of::<TilesConstants>() as u32,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            MiscFlags: 0,
            StructureByteStride: 0,
        };

        let mut buffer_out = None;
        let result = device.CreateBuffer(&buffer_desc, None, Some(&mut buffer_out));
        if let Err(e) = result {
            println!("ERROR creating constant buffer: {:?}", e);
            println!(
                "Buffer size: {} bytes",
                std::mem::size_of::<TilesConstants>()
            );
            return Err(e);
        }
        buffer_out.ok_or(E_POINTER)?
    };
    println!("Constant buffer created successfully");

    // Add tiles shader to the list
    pixel_shaders.push(PixelShaderConfig {
        name: "tiles".to_string(),
        shader_type: ShaderType::Tiles {
            shader: tiles_shader,
            spritesheet_srv: sheet_srv,
            brightness_srv,
            constants_buffer: tiles_constants_buffer,
            sheet_width: sheet_w,
            sheet_height: sheet_h,
            tiles_per_row,
            total_tiles: brightness.len(),
        },
    });
    println!("tiles shader ready");

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

    let haccel = create_accelerators()?;

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

            if TranslateAcceleratorW(hwnd, *haccel, &message) != 0 {
                continue;
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

const ID_SAVE: u16 = 1001;
const ID_ALWAYS_ON_TOP: u16 = 1002;
const ID_TOGGLE_PAUSE: u16 = 1003;
const ID_SHADER_BASE: u16 = 2000;
const ID_SHADER_END: u16 = ID_SHADER_BASE + 10;

fn create_accelerators() -> Result<Owned<HACCEL>> {
    let accels = [
        ACCEL {
            fVirt: FCONTROL | FVIRTKEY,
            key: b'S' as u16,
            cmd: ID_SAVE,
        },
        ACCEL {
            fVirt: FCONTROL | FVIRTKEY,
            key: b'A' as u16,
            cmd: ID_ALWAYS_ON_TOP,
        },
        ACCEL {
            fVirt: FVIRTKEY,
            key: 19, // VK_PAUSE
            cmd: ID_TOGGLE_PAUSE,
        },
        ACCEL {
            fVirt: FVIRTKEY,
            key: b'1' as u16,
            cmd: ID_SHADER_BASE,
        },
        ACCEL {
            fVirt: FVIRTKEY,
            key: b'2' as u16,
            cmd: ID_SHADER_BASE + 1,
        },
        ACCEL {
            fVirt: FVIRTKEY,
            key: b'3' as u16,
            cmd: ID_SHADER_BASE + 2,
        },
        ACCEL {
            fVirt: FVIRTKEY,
            key: b'4' as u16,
            cmd: ID_SHADER_BASE + 3,
        },
        ACCEL {
            fVirt: FVIRTKEY,
            key: b'5' as u16,
            cmd: ID_SHADER_BASE + 4,
        },
        ACCEL {
            fVirt: FVIRTKEY,
            key: b'6' as u16,
            cmd: ID_SHADER_BASE + 5,
        },
        ACCEL {
            fVirt: FVIRTKEY,
            key: b'7' as u16,
            cmd: ID_SHADER_BASE + 6,
        },
        ACCEL {
            fVirt: FVIRTKEY,
            key: b'8' as u16,
            cmd: ID_SHADER_BASE + 7,
        },
        ACCEL {
            fVirt: FVIRTKEY,
            key: b'9' as u16,
            cmd: ID_SHADER_BASE + 8,
        },
    ];

    unsafe { CreateAcceleratorTableW(&accels).map(|h| Owned::new(h)) }
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
            WM_COMMAND => {
                let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut CaptureState;
                if !state_ptr.is_null() {
                    let state = &mut *state_ptr;
                    let accel_id = (wparam.0 & 0xFFFF) as u16;
                    match accel_id {
                        ID_SAVE => {
                            if let Err(e) = save_frame_to_png(state) {
                                println!("Failed to save frame: {:?}", e);
                            }
                        }
                        ID_ALWAYS_ON_TOP => {
                            if let Err(e) = toggle_always_on_top(state) {
                                println!("Failed to toggle always on top: {:?}", e);
                            }
                        }
                        ID_TOGGLE_PAUSE => {
                            if let Err(e) = toggle_pause_and_hide(state) {
                                println!("Failed to toggle pause and hide: {:?}", e);
                            }
                        }
                        ID_SHADER_BASE..ID_SHADER_END => {
                            // Number keys for shader switching
                            let idx = (accel_id - ID_SHADER_BASE) as usize;
                            if idx < state.pixel_shaders.len() {
                                println!("Switched to {} shader", state.pixel_shaders[idx].name);
                                state.current_shader = idx
                            }
                        }
                        _ => {}
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

fn load_png_from_bytes(
    device: &ID3D11Device,
    png_bytes: &[u8],
    name: &str,
) -> Result<(ID3D11Texture2D, ID3D11ShaderResourceView, u32, u32, Vec<u8>)> {
    unsafe {
        // Create WIC factory
        let wic_factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;

        // Create a stream from the embedded PNG bytes
        let Some(stream) = SHCreateMemStream(Some(png_bytes)) else {
            return Err(Error::from_thread());
        };

        // Create decoder from stream
        let decoder = wic_factory.CreateDecoderFromStream(
            &stream,
            std::ptr::null(),
            WICDecodeMetadataCacheOnDemand,
        )?;

        // Get the first frame
        let frame = decoder.GetFrame(0)?;

        // Get frame dimensions
        let mut width = 0u32;
        let mut height = 0u32;
        frame.GetSize(&mut width, &mut height)?;

        // Convert to BGRA format
        let target_format = GUID_WICPixelFormat32bppBGRA;
        let converter = wic_factory.CreateFormatConverter()?;
        converter.Initialize(
            &frame,
            &target_format,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeMedianCut,
        )?;

        // Calculate stride and buffer size
        let stride = width * 4; // 4 bytes per pixel (BGRA)
        let buffer_size = stride * height;

        // Read pixels into buffer
        let mut pixel_buffer = vec![0u8; buffer_size as usize];
        converter.CopyPixels(std::ptr::null(), stride, &mut pixel_buffer)?;

        // Create D3D11 texture with initial data
        let texture_desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
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

        let texture_data = D3D11_SUBRESOURCE_DATA {
            pSysMem: pixel_buffer.as_ptr() as *const _,
            SysMemPitch: stride,
            SysMemSlicePitch: 0,
        };

        let mut texture = None;
        device.CreateTexture2D(&texture_desc, Some(&texture_data), Some(&mut texture))?;
        let texture = texture.ok_or(E_POINTER)?;

        // Create shader resource view
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

        let mut srv = None;
        device.CreateShaderResourceView(&texture, Some(&srv_desc), Some(&mut srv))?;
        let srv = srv.ok_or(E_POINTER)?;

        println!(
            "Loaded {} ({}x{}, {} bytes)",
            name, width, height, buffer_size
        );

        Ok((texture, srv, width, height, pixel_buffer))
    }
}

fn compute_tile_brightness(
    pixels: &[u8],
    width: u32,
    height: u32,
    tile_width: u32,
    tile_height: u32,
) -> Vec<f32> {
    let tiles_per_row = width / tile_width;
    let tiles_per_col = height / tile_height;
    let total_tiles = tiles_per_row * tiles_per_col;

    let mut brightness_values = Vec::with_capacity(total_tiles as usize);

    for tile_row in 0..tiles_per_col {
        for tile_col in 0..tiles_per_row {
            let mut brightness_sum = 0.0f32;

            // Sample the tile
            for sy in 0..tile_height {
                for sx in 0..tile_width {
                    let pixel_x = tile_col * tile_width + sx;
                    let pixel_y = tile_row * tile_height + sy;

                    // Get pixel index (BGRA format)
                    let pixel_index = ((pixel_y * width + pixel_x) * 4) as usize;

                    if pixel_index + 2 < pixels.len() {
                        let b = pixels[pixel_index] as f32 / 255.0;
                        let g = pixels[pixel_index + 1] as f32 / 255.0;
                        let r = pixels[pixel_index + 2] as f32 / 255.0;

                        // Compute luminance using standard coefficients
                        let luminance = 0.299 * r + 0.587 * g + 0.114 * b;
                        brightness_sum += luminance;
                    }
                }
            }

            // Average brightness for this tile
            let avg_brightness = brightness_sum / (tile_width * tile_height) as f32;
            brightness_values.push(avg_brightness);
        }
    }

    brightness_values
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
        state
            .context
            .PSSetSamplers(0, Some(&[Some(state.sampler.clone())]));

        // Bind resources based on shader type
        match &state.pixel_shaders[state.current_shader].shader_type {
            ShaderType::Simple(shader) => {
                state.context.PSSetShader(shader, None);
                // Use the extended texture instead of staging texture
                state.context.PSSetShaderResources(
                    0,
                    Some(&[Some(state.extended_srv.as_ref().unwrap().clone())]),
                );
            }
            ShaderType::Tiles {
                shader,
                spritesheet_srv,
                brightness_srv,
                constants_buffer,
                sheet_width,
                sheet_height,
                tiles_per_row,
                total_tiles,
            } => {
                state.context.PSSetShader(shader, None);

                // Bind 3 shader resources: t0 = source, t1 = spritesheet, t2 = brightness
                state.context.PSSetShaderResources(
                    0,
                    Some(&[
                        Some(state.extended_srv.as_ref().unwrap().clone()),
                        Some(spritesheet_srv.clone()),
                        Some(brightness_srv.clone()),
                    ]),
                );

                // Update constant buffer with current source resolution
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                state.context.Map(
                    constants_buffer,
                    0,
                    D3D11_MAP_WRITE_DISCARD,
                    0,
                    Some(&mut mapped),
                )?;

                let constants = TilesConstants {
                    source_resolution: [extended_width as f32, extended_height as f32],
                    tile_size: [8.0, 16.0],
                    tiles_per_row: *tiles_per_row as i32,
                    total_tiles: *total_tiles as i32,
                    spritesheet_resolution: [*sheet_width as f32, *sheet_height as f32],
                };

                // Debug: print constants once
                static mut PRINTED: bool = false;
                if !PRINTED {
                    println!("Tiles shader constants:");
                    println!("  source_resolution: {:?}", constants.source_resolution);
                    println!("  tile_size: {:?}", constants.tile_size);
                    println!("  tiles_per_row: {}", constants.tiles_per_row);
                    println!("  total_tiles: {}", constants.total_tiles);
                    println!(
                        "  spritesheet_resolution: {:?}",
                        constants.spritesheet_resolution
                    );
                    PRINTED = true;
                }

                std::ptr::copy_nonoverlapping(
                    &constants as *const _ as *const u8,
                    mapped.pData as *mut u8,
                    std::mem::size_of::<TilesConstants>(),
                );
                state.context.Unmap(constants_buffer, 0);

                // Bind constant buffer to b0
                state
                    .context
                    .PSSetConstantBuffers(0, Some(&[Some(constants_buffer.clone())]));
            }
        }

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
