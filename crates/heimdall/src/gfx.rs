//! Rendering: Direct3D 11 for the glass, Direct2D/DirectWrite for content,
//! presented through a DirectComposition swap chain with premultiplied alpha
//! so the window can be any shape with a real soft shadow.

use windows::core::{s, Interface, Result, BOOL, HSTRING, PCSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::Fxc::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows_numerics::{Matrix3x2, Vector2};

const SHADER: &str = include_str!("glass.hlsl");

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Color {
        Color { r, g, b, a }
    }
    pub fn alpha(self, a: f32) -> Color {
        Color { a: self.a * a, ..self }
    }
    fn d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F { r: self.r, g: self.g, b: self.b, a: self.a }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    fn d2d(self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x, top: self.y, right: self.x + self.w, bottom: self.y + self.h }
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

/// Constant buffer layout; must match `Params` in glass.hlsl.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct GlassParams {
    pub size: [f32; 2],
    pub scale: f32,
    pub dark: f32,
    pub shape: [f32; 4],
    pub sel: [f32; 4],
    pub tint: [f32; 4],
    pub accent: [f32; 4],
    pub radius: f32,
    pub sel_radius: f32,
    pub sel_alpha: f32,
    pub opacity: f32,
    pub light: [f32; 2],
    pub backdrop: f32,
    pub lens: f32,
    pub back_size: [f32; 2],
    pub blur_lod: f32,
    /// Shadow strength, 0 (off) to 1 (normal).
    pub shadow: f32,
}

pub struct Fonts {
    pub query: IDWriteTextFormat,
    pub title: IDWriteTextFormat,
    pub subtitle: IDWriteTextFormat,
    pub hint: IDWriteTextFormat,
    pub header: IDWriteTextFormat,
    pub glyph: IDWriteTextFormat,
    pub glyph_row: IDWriteTextFormat,
    pub mono: IDWriteTextFormat,
    pub preview_title: IDWriteTextFormat,
    pub body: IDWriteTextFormat,
    pub menu: IDWriteTextFormat,
    pub menu_glyph: IDWriteTextFormat,
}

pub struct Gfx {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    swap: IDXGISwapChain1,
    _dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    pub dc: ID2D1DeviceContext,
    pub dwrite: IDWriteFactory,
    pub fonts: Fonts,
    brush: ID2D1SolidColorBrush,
    target: Option<(ID2D1Bitmap1, ID3D11RenderTargetView)>,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cb: ID3D11Buffer,
    sampler: ID3D11SamplerState,
    backdrop: Option<ID3D11ShaderResourceView>,
    blank: ID3D11ShaderResourceView,
    waitable: HANDLE,
    pub size: (u32, u32),
}

impl Drop for Gfx {
    fn drop(&mut self) {
        if !self.waitable.is_invalid() {
            unsafe { CloseHandle(self.waitable).ok() };
        }
    }
}

fn compile(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob> {
    let mut code = None;
    let mut errors = None;
    let r = unsafe {
        D3DCompile(
            SHADER.as_ptr() as *const _,
            SHADER.len(),
            s!("glass.hlsl"),
            None,
            None,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = r {
        if let Some(err) = errors {
            let msg = unsafe {
                std::slice::from_raw_parts(err.GetBufferPointer() as *const u8, err.GetBufferSize())
            };
            crate::log(format!("shader: {}", String::from_utf8_lossy(msg)));
        }
        return Err(e);
    }
    Ok(code.unwrap())
}

fn blob_bytes(b: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()) }
}

fn text_format(
    dw: &IDWriteFactory,
    families: &[&str],
    weight: DWRITE_FONT_WEIGHT,
    size: f32,
) -> Result<IDWriteTextFormat> {
    // Use the first family that is installed (Segoe UI Variable ships with Windows 11).
    let mut collection = None;
    unsafe { dw.GetSystemFontCollection(&mut collection, false)? };
    let collection = collection.unwrap();
    let mut family = families[families.len() - 1];
    for f in families {
        let mut idx = 0;
        let mut exists = BOOL(0);
        unsafe { collection.FindFamilyName(&HSTRING::from(*f), &mut idx, &mut exists)? };
        if exists.as_bool() {
            family = f;
            break;
        }
    }
    let fmt = unsafe {
        dw.CreateTextFormat(
            &HSTRING::from(family),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            &HSTRING::from("en-us"),
        )?
    };
    unsafe {
        fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        let sign = dw.CreateEllipsisTrimmingSign(&fmt)?;
        let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
        fmt.SetTrimming(&trim, &sign)?;
    }
    Ok(fmt)
}

impl Gfx {
    pub fn new(hwnd: HWND, w: u32, h: u32) -> Result<Gfx> {
        unsafe {
            let mut device = None;
            let mut ctx = None;
            let levels = [D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0];
            let mut r = Err(windows::core::Error::empty());
            for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
                r = D3D11CreateDevice(
                    None,
                    driver,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    Some(&levels),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut ctx),
                );
                if r.is_ok() {
                    break;
                }
            }
            r?;
            let device: ID3D11Device = device.unwrap();
            let ctx: ID3D11DeviceContext = ctx.unwrap();
            let dxgi: IDXGIDevice = device.cast()?;
            let factory: IDXGIFactory2 = dxgi.GetAdapter()?.GetParent()?;

            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: w.max(1),
                Height: h.max(1),
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                Flags: DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32,
                ..Default::default()
            };
            let swap = factory.CreateSwapChainForComposition(&device, &desc, None)?;
            // One frame in flight; the waitable tells the message loop when the
            // next frame can start, so Present never blocks the UI thread.
            let swap2: IDXGISwapChain2 = swap.cast()?;
            swap2.SetMaximumFrameLatency(1)?;
            let waitable = swap2.GetFrameLatencyWaitableObject();

            let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
            let target = dcomp.CreateTargetForHwnd(hwnd, true)?;
            let visual = dcomp.CreateVisual()?;
            visual.SetContent(&swap)?;
            target.SetRoot(&visual)?;
            dcomp.Commit()?;

            let d2d: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let d2d_device = d2d.CreateDevice(&dxgi)?;
            let dc = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let brush = dc.CreateSolidColorBrush(&Color::rgba(1.0, 1.0, 1.0, 1.0).d2d(), None)?;

            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let display = ["Segoe UI Variable Display", "Segoe UI"];
            let text = ["Segoe UI Variable Text", "Segoe UI"];
            let small = ["Segoe UI Variable Small", "Segoe UI"];
            let icons = ["Segoe Fluent Icons", "Segoe MDL2 Assets"];
            let fonts = Fonts {
                query: text_format(&dwrite, &display, DWRITE_FONT_WEIGHT_NORMAL, 21.0)?,
                title: text_format(&dwrite, &text, DWRITE_FONT_WEIGHT_NORMAL, 14.0)?,
                subtitle: text_format(&dwrite, &small, DWRITE_FONT_WEIGHT_NORMAL, 11.5)?,
                hint: text_format(&dwrite, &small, DWRITE_FONT_WEIGHT_NORMAL, 11.5)?,
                header: text_format(&dwrite, &small, DWRITE_FONT_WEIGHT_SEMI_BOLD, 11.0)?,
                glyph: text_format(&dwrite, &icons, DWRITE_FONT_WEIGHT_NORMAL, 17.0)?,
                glyph_row: text_format(&dwrite, &icons, DWRITE_FONT_WEIGHT_NORMAL, 18.0)?,
                mono: text_format(&dwrite, &["Cascadia Mono", "Consolas"], DWRITE_FONT_WEIGHT_NORMAL, 11.0)?,
                preview_title: text_format(&dwrite, &display, DWRITE_FONT_WEIGHT_SEMI_BOLD, 16.0)?,
                body: text_format(&dwrite, &small, DWRITE_FONT_WEIGHT_NORMAL, 11.5)?,
                menu: text_format(&dwrite, &text, DWRITE_FONT_WEIGHT_NORMAL, 13.0)?,
                menu_glyph: text_format(&dwrite, &icons, DWRITE_FONT_WEIGHT_NORMAL, 15.0)?,
            };
            fonts.glyph.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            fonts.glyph_row.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            fonts.menu_glyph.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            fonts.hint.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
            fonts.mono.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            fonts.mono.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            fonts.body.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;

            let vsb = compile(s!("vs"), s!("vs_4_0"))?;
            let psb = compile(s!("ps"), s!("ps_4_0"))?;
            let mut vs = None;
            device.CreateVertexShader(blob_bytes(&vsb), None, Some(&mut vs))?;
            let mut ps = None;
            device.CreatePixelShader(blob_bytes(&psb), None, Some(&mut ps))?;

            let cbd = D3D11_BUFFER_DESC {
                ByteWidth: std::mem::size_of::<GlassParams>().next_multiple_of(16) as u32,
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                ..Default::default()
            };
            let mut cb = None;
            device.CreateBuffer(&cbd, None, Some(&mut cb))?;

            let sd = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            let mut sampler = None;
            device.CreateSamplerState(&sd, Some(&mut sampler))?;

            let blank = Self::make_texture(&device, &ctx, &[0, 0, 0, 255], 1, 1)?;

            Ok(Gfx {
                device,
                ctx,
                swap,
                _dcomp: dcomp,
                _target: target,
                _visual: visual,
                dc,
                dwrite,
                fonts,
                brush,
                target: None,
                vs: vs.unwrap(),
                ps: ps.unwrap(),
                cb: cb.unwrap(),
                sampler: sampler.unwrap(),
                backdrop: None,
                blank,
                waitable,
                size: (w.max(1), h.max(1)),
            })
        }
    }

    fn make_texture(
        device: &ID3D11Device,
        ctx: &ID3D11DeviceContext,
        bgra: &[u8],
        w: u32,
        h: u32,
    ) -> Result<ID3D11ShaderResourceView> {
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 0,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: D3D11_RESOURCE_MISC_GENERATE_MIPS.0 as u32,
            };
            let mut tex = None;
            device.CreateTexture2D(&desc, None, Some(&mut tex))?;
            let tex = tex.unwrap();
            ctx.UpdateSubresource(&tex, 0, None, bgra.as_ptr() as *const _, w * 4, 0);
            let mut srv = None;
            device.CreateShaderResourceView(&tex, None, Some(&mut srv))?;
            let srv = srv.unwrap();
            ctx.GenerateMips(&srv);
            Ok(srv)
        }
    }

    /// Upload a freshly captured backdrop (BGRA, top-down). `None` clears it.
    pub fn set_backdrop(&mut self, img: Option<(&[u8], u32, u32)>) {
        self.backdrop = img.and_then(|(px, w, h)| Self::make_texture(&self.device, &self.ctx, px, w, h).ok());
    }

    /// Signalled when the compositor is ready for the next frame.
    pub fn waitable(&self) -> HANDLE {
        self.waitable
    }

    pub fn has_backdrop(&self) -> bool {
        self.backdrop.is_some()
    }

    pub fn resize(&mut self, w: u32, h: u32) -> Result<()> {
        let (w, h) = (w.max(1), h.max(1));
        if (w, h) == self.size {
            return Ok(());
        }
        self.target = None;
        unsafe {
            self.dc.SetTarget(None);
            self.ctx.OMSetRenderTargets(None, None);
            self.ctx.ClearState();
            self.ctx.Flush();
            self.swap.ResizeBuffers(2, w, h, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT)?;
        }
        self.size = (w, h);
        Ok(())
    }

    fn ensure_target(&mut self) -> Result<()> {
        if self.target.is_some() {
            return Ok(());
        }
        unsafe {
            let tex: ID3D11Texture2D = self.swap.GetBuffer(0)?;
            let mut rtv = None;
            self.device.CreateRenderTargetView(&tex, None, Some(&mut rtv))?;
            let surface: IDXGISurface = tex.cast()?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let bmp = self.dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
            self.target = Some((bmp, rtv.unwrap()));
        }
        Ok(())
    }

    /// Draw the glass layer, then let `content` draw on top with Direct2D (in DIPs).
    pub fn frame(&mut self, mut params: GlassParams, dpi: f32, content: impl FnOnce(&Gfx)) -> Result<()> {
        self.ensure_target()?;
        let (bmp, rtv) = self.target.clone().unwrap();
        unsafe {
            params.size = [self.size.0 as f32, self.size.1 as f32];
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(&self.cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))?;
            std::ptr::copy_nonoverlapping(&params, mapped.pData as *mut GlassParams, 1);
            self.ctx.Unmap(&self.cb, 0);

            self.ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            self.ctx.ClearRenderTargetView(&rtv, &[0.0, 0.0, 0.0, 0.0]);
            let vp = D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: self.size.0 as f32,
                Height: self.size.1 as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            self.ctx.RSSetViewports(Some(&[vp]));
            self.ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.ctx.IASetInputLayout(None);
            self.ctx.VSSetShader(&self.vs, None);
            self.ctx.PSSetShader(&self.ps, None);
            self.ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            let srv = self.backdrop.clone().unwrap_or_else(|| self.blank.clone());
            self.ctx.PSSetShaderResources(0, Some(&[Some(srv)]));
            self.ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            self.ctx.Draw(3, 0);
            self.ctx.OMSetRenderTargets(None, None);

            self.dc.SetTarget(&bmp);
            self.dc.SetDpi(dpi, dpi);
            self.dc.BeginDraw();
            self.dc.SetTransform(&Matrix3x2::identity());
            content(self);
            let r = self.dc.EndDraw(None, None);
            self.dc.SetTarget(None);
            r?;
            let t0 = std::time::Instant::now();
            // Paced by the frame-latency waitable, so no vsync wait here.
            self.swap.Present(0, DXGI_PRESENT(0)).ok()?;
            if crate::trace() {
                let ms = t0.elapsed().as_secs_f64() * 1e3;
                if ms > 2.0 {
                    crate::log(format!("present blocked {ms:.2} ms"));
                }
            }
        }
        Ok(())
    }

    // ---- drawing helpers (DIPs) ------------------------------------------------

    fn set_color(&self, c: Color) {
        unsafe { self.brush.SetColor(&c.d2d()) };
    }

    pub fn fill_round(&self, r: Rect, radius: f32, c: Color) {
        self.set_color(c);
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.dc.FillRoundedRectangle(&rr, &self.brush) };
    }

    pub fn stroke_round(&self, r: Rect, radius: f32, c: Color) {
        self.set_color(c);
        let w = 1.0 / self.dpi_scale();
        let inset = Rect::new(r.x + w / 2.0, r.y + w / 2.0, r.w - w, r.h - w);
        let rr = D2D1_ROUNDED_RECT { rect: inset.d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.dc.DrawRoundedRectangle(&rr, &self.brush, w, None) };
    }

    pub fn fill(&self, r: Rect, c: Color) {
        self.set_color(c);
        unsafe { self.dc.FillRectangle(&r.d2d(), &self.brush) };
    }

    pub fn hline(&self, x0: f32, x1: f32, y: f32, c: Color) {
        self.fill(Rect::new(x0, y, x1 - x0, 1.0 / (self.dpi_scale())), c);
    }

    pub fn vline(&self, x: f32, y0: f32, y1: f32, c: Color) {
        self.fill(Rect::new(x, y0, 1.0 / (self.dpi_scale()), y1 - y0), c);
    }

    fn dpi_scale(&self) -> f32 {
        let (mut x, mut y) = (96.0, 96.0);
        unsafe { self.dc.GetDpi(&mut x, &mut y) };
        x / 96.0
    }

    pub fn text(&self, s: &str, fmt: &IDWriteTextFormat, r: Rect, c: Color) {
        if s.is_empty() {
            return;
        }
        self.set_color(c);
        let w: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            self.dc.DrawText(
                &w,
                fmt,
                &r.d2d(),
                &self.brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            )
        };
    }

    pub fn layout(&self, s: &str, fmt: &IDWriteTextFormat, w: f32, h: f32) -> Option<IDWriteTextLayout> {
        let ws: Vec<u16> = s.encode_utf16().collect();
        unsafe { self.dwrite.CreateTextLayout(&ws, fmt, w, h).ok() }
    }

    /// Draw a layout with some UTF-16 ranges emphasised (search-match highlight).
    pub fn text_highlighted(
        &self,
        layout: &IDWriteTextLayout,
        at: (f32, f32),
        c: Color,
        ranges: &[(u32, u32)],
        hc: Color,
    ) {
        unsafe {
            if !ranges.is_empty() {
                if let Ok(hb) = self.dc.CreateSolidColorBrush(&hc.d2d(), None) {
                    for &(start, len) in ranges {
                        let tr = DWRITE_TEXT_RANGE { startPosition: start, length: len };
                        layout.SetDrawingEffect(&hb, tr).ok();
                        layout.SetFontWeight(DWRITE_FONT_WEIGHT_BOLD, tr).ok();
                    }
                }
            }
            self.set_color(c);
            self.dc.DrawTextLayout(Vector2 { X: at.0, Y: at.1 }, layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_CLIP);
        }
    }

    pub fn draw_layout(&self, layout: &IDWriteTextLayout, at: (f32, f32), c: Color) {
        self.set_color(c);
        unsafe {
            self.dc.DrawTextLayout(Vector2 { X: at.0, Y: at.1 }, layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_CLIP)
        };
    }

    pub fn bitmap(&self, b: &ID2D1Bitmap1, r: Rect, opacity: f32) {
        unsafe {
            self.dc.DrawBitmap(
                b,
                Some(&r.d2d()),
                opacity,
                D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                None,
                None,
            )
        };
    }

    pub fn create_bitmap(&self, bgra: &[u8], w: u32, h: u32) -> Option<ID2D1Bitmap1> {
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        unsafe {
            self.dc
                .CreateBitmap(D2D_SIZE_U { width: w, height: h }, Some(bgra.as_ptr() as *const _), w * 4, &props)
                .ok()
        }
    }

    pub fn push_clip(&self, r: Rect) {
        unsafe { self.dc.PushAxisAlignedClip(&r.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
    }

    pub fn pop_clip(&self) {
        unsafe { self.dc.PopAxisAlignedClip() };
    }

    pub fn push_opacity(&self, opacity: f32) {
        let params = D2D1_LAYER_PARAMETERS1 {
            contentBounds: D2D_RECT_F { left: -1e6, top: -1e6, right: 1e6, bottom: 1e6 },
            geometricMask: std::mem::ManuallyDrop::new(None),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: Matrix3x2::identity(),
            opacity,
            opacityBrush: std::mem::ManuallyDrop::new(None),
            layerOptions: D2D1_LAYER_OPTIONS1_NONE,
        };
        unsafe { self.dc.PushLayer(&params, None) };
    }

    pub fn pop_layer(&self) {
        unsafe { self.dc.PopLayer() };
    }

    pub fn set_transform(&self, scale: f32, cx: f32, cy: f32) {
        let m = Matrix3x2::scale_around(scale, scale, Vector2 { X: cx, Y: cy });
        unsafe { self.dc.SetTransform(&m) };
    }

    pub fn reset_transform(&self) {
        unsafe { self.dc.SetTransform(&Matrix3x2::identity()) };
    }
}

/// Caret x-position (DIPs) for a UTF-16 index inside a layout.
pub fn caret_x(layout: &IDWriteTextLayout, pos16: u32) -> f32 {
    let (mut x, mut y) = (0.0f32, 0.0f32);
    let mut m = DWRITE_HIT_TEST_METRICS::default();
    unsafe { layout.HitTestTextPosition(pos16, false, &mut x, &mut y, &mut m).ok() };
    x
}

pub fn layout_width(layout: &IDWriteTextLayout) -> f32 {
    let mut m = DWRITE_TEXT_METRICS::default();
    unsafe { layout.GetMetrics(&mut m).ok() };
    m.widthIncludingTrailingWhitespace
}
