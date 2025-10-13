use windows::{
    Win32::{
        Foundation::*,
        Graphics::{
            Direct3D::Fxc::*, Direct3D::*, Direct3D11::*, Dxgi::Common::*, Dxgi::*, Gdi::*,
        },
        System::Com::*,
        System::LibraryLoader::*,
        UI::WindowsAndMessaging::*,
    },
    core::*,
};

struct CaptureState {
    start_time: std::time::Instant,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    swap_chain: IDXGISwapChain1,
    dxgi_adapter: IDXGIAdapter,
    duplication: Option<IDXGIOutputDuplication>,
    vertex_shader: ID3D11VertexShader,
    pixel_shader: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    vertex_buffer: ID3D11Buffer,
    render_target_view: Option<ID3D11RenderTargetView>,
    shader_resource_view: Option<ID3D11ShaderResourceView>,
    input_layout: ID3D11InputLayout,
    time_buffer: ID3D11Buffer,

    staging_texture: Option<ID3D11Texture2D>,
    source_rect: RECT,
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

const PIXEL_SHADER_PASSTHRU: &[u8] = b"
Texture2D screenTexture : register(t0);
SamplerState texSampler : register(s0);

float4 main(float4 pos : SV_POSITION, float2 texCoord : TEXCOORD) : SV_Target {
    return screenTexture.Sample(texSampler, texCoord);
}";

const PIXEL_SHADER: &[u8] = b"
Texture2D screenTexture : register(t0);
SamplerState texSampler : register(s0);
cbuffer TimeBuffer : register(b0) {
    float Time;
    float3 padding;
}

float4 main(float4 pos : SV_POSITION, float2 texCoord : TEXCOORD) : SV_Target {
    float2 wavyCoord = texCoord;
    wavyCoord.x += sin(texCoord.y * 10.0f + Time) * 0.02f;
    wavyCoord.y += cos(texCoord.x * 10.0f + Time) * 0.02f;
    return screenTexture.Sample(texSampler, wavyCoord);
}
";

fn main() -> Result<()> {
    unsafe {
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
        let mut shader_blob = None;
        let mut error_blob = None;
        let res = D3DCompile(
            VERTEX_SHADER.as_ptr() as *const _,
            VERTEX_SHADER.len(),
            PCSTR::null(),                                   // source name (optional)
            None,                                            // defines (optional)
            None,                                            // include handler (optional)
            s!("main"),                                      // entry point
            s!("vs_4_0"),                                    // target profile
            D3DCOMPILE_DEBUG | D3DCOMPILE_SKIP_OPTIMIZATION, // compilation flags
            0,                                               // secondary flags
            &mut shader_blob,                                // output blob
            Some(&mut error_blob),                           // error blob
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

    let pixel_shader = unsafe {
        let mut shader_blob: Option<ID3DBlob> = None;
        let mut error_blob = None;
        let res = D3DCompile(
            PIXEL_SHADER.as_ptr() as *const _,
            PIXEL_SHADER.len(),
            None,                                            // source name (optional)
            None,                                            // defines (optional)
            None,                                            // include handler (optional)
            s!("main"),                                      // entry point
            s!("ps_4_0"),                                    // target profile
            D3DCOMPILE_DEBUG | D3DCOMPILE_SKIP_OPTIMIZATION, // compilation flags
            0,                                               // secondary flags
            &mut shader_blob,                                // output blob
            Some(&mut error_blob),                           // error blob
        );
        println!("pixel shader compilation complete {:?}", res);

        if let Some(error) = error_blob {
            let error_message =
                std::str::from_utf8(blob_as_slice(&error)).unwrap_or("Unknown error");
            println!("Shader compilation error: {}", error_message);
        }

        res?;

        let Some(blob) = shader_blob else {
            return Err(Error::new(E_FAIL, "Failed to compile pixel shader"));
        };

        let mut shader_out = None;
        device.CreatePixelShader(blob_as_slice(&blob), None, Some(&mut shader_out))?;
        shader_out.ok_or(E_POINTER)?
    };
    println!("created pixel shader");

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
        pixel_shader,
        sampler,
        vertex_buffer,
        render_target_view: None,
        shader_resource_view: None,
        input_layout,
        time_buffer,
        staging_texture: None,
        source_rect: RECT::default(),
    };
    println!("created capture state");

    unsafe {
        SetWindowLongPtrW(
            hwnd,
            GWLP_USERDATA,
            Box::into_raw(Box::new(capture_state)) as isize,
        );

        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
    }

    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
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
                    ClientToScreen(hwnd, &mut client_origin);
                    let mut client_rect = RECT::default();
                    GetClientRect(hwnd, &mut client_rect);
                    let mut source_rect = client_rect;
                    source_rect.left += client_origin.x;
                    source_rect.right += client_origin.x;
                    source_rect.top += client_origin.y;
                    source_rect.bottom += client_origin.y;
                    state.source_rect = source_rect;

                    if message == WM_SIZE {
                        state.render_target_view = None;
                        state.staging_texture = None; // Recreate on size change
                        if let Err(_) = resize_swapchain(state, hwnd) {
                            // Handle error if needed
                        }
                    }
                }
                LRESULT(0)
            }
            WM_PAINT => {
                let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut CaptureState;
                if !state_ptr.is_null() {
                    let state = &mut *state_ptr;
                    if let Err(e) = capture_and_render_frame(state, hwnd) {
                        // Handle error if needed
                        println!("error {:?}", e);
                        if e.code() == DXGI_ERROR_ACCESS_LOST {
                            state.duplication = None;
                        }
                    }
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
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

            let mut texture = None;
            state
                .device
                .CreateTexture2D(&desc, None, Some(&mut texture))?;
            state.staging_texture = texture;
        }

        // Copy the region under the window
        let texture: ID3D11Texture2D = frame_texture.cast()?;
        let dst_texture = state.staging_texture.as_ref().unwrap();

        let src_box = D3D11_BOX {
            left: state.source_rect.left as u32,
            top: state.source_rect.top as u32,
            front: 0,
            right: (state.source_rect.left + width) as u32,
            bottom: (state.source_rect.top + height) as u32,
            back: 1,
        };

        state
            .context
            .CopySubresourceRegion(dst_texture, 0, 0, 0, 0, &texture, 0, Some(&src_box));

        // Create shader resource view if needed
        if state.shader_resource_view.is_none() {
            let mut shader_resource_view = None;
            let resource: ID3D11Resource = dst_texture.cast()?;

            let texture: ID3D11Texture2D = resource.cast()?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            texture.GetDesc(&mut desc);

            println!("texture format {:?}", desc.Format);

            let srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
                Format: desc.Format,
                ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_SRV {
                        MostDetailedMip: 0,
                        MipLevels: 1,
                    },
                },
            };

            state.device.CreateShaderResourceView(
                &resource,
                Some(&srv_desc),
                Some(&mut shader_resource_view),
            )?;

            if shader_resource_view.is_none() {
                return Err(Error::new(E_FAIL, "failed to create shader resource view"));
            }
            state.shader_resource_view = shader_resource_view;
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
        state.context.PSSetShader(&state.pixel_shader, None);
        state
            .context
            .PSSetSamplers(0, Some(&[Some(state.sampler.clone())]));
        state.context.PSSetShaderResources(
            0,
            Some(&[Some(state.shader_resource_view.as_ref().unwrap().clone())]),
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
                if frame.info.LastPresentTime != 0 {
                    if let Some(frame_texture) = frame.resource.clone() {
                        handle_frame(state, frame_texture, hwnd)?;
                    }
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

fn blob_as_slice(blob: &ID3DBlob) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize())
    }
}
