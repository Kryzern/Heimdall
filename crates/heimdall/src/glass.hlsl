// Heimdall glass.
//
// One full-screen triangle, one pixel shader, drawn only when something
// changes. The desktop behind the panel is captured once per open, mip-mapped,
// and sampled here. Everything "liquid" happens at the rim: the background is
// refracted outward along the panel's SDF normal, red and blue split slightly,
// and a specular line follows a light that tracks the selection. The middle of
// the panel stays calm so text reads cleanly.

cbuffer Params : register(b0)
{
    float2 uSize;       // render target size, px
    float  uScale;      // DPI scale (1.0 = 96 dpi)
    float  uDark;       // 1 = dark theme
    float4 uShape;      // panel x, y, w, h (px)
    float4 uSel;        // selection droplet x, y, w, h (px)
    float4 uTint;       // rgb, strength
    float4 uAccent;     // rgb, unused
    float  uRadius;     // panel corner radius (px)
    float  uSelRadius;  // droplet corner radius (px)
    float  uSelAlpha;   // droplet visibility
    float  uOpacity;    // whole-panel fade
    float2 uLight;      // unit vector pointing toward the light
    float  uBackdrop;   // 1 when a captured backdrop is bound
    float  uLens;       // rim refraction distance (px at 1.0 scale)
    float2 uBackSize;   // backdrop texture size (px)
    float  uBlurLod;    // mip level used for the frost
    float  uPad;
};

Texture2D    gBack : register(t0);
SamplerState gSamp : register(s0);

struct VSOut { float4 pos : SV_Position; };

VSOut vs(uint id : SV_VertexID)
{
    VSOut o;
    float2 uv = float2((id << 1) & 2, id & 2);
    o.pos = float4(uv * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0);
    return o;
}

float sdRound(float2 p, float2 c, float2 h, float r)
{
    float2 q = abs(p - c) - h + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

// 9-tap tent over a mip level: a soft blur for the cost of nine fetches.
float3 frost(float2 px, float lod)
{
    float2 uv = px / uBackSize;
    float2 t = exp2(lod) * 0.9 / uBackSize;
    float3 s = gBack.SampleLevel(gSamp, uv, lod).rgb * 4.0;
    s += gBack.SampleLevel(gSamp, uv + float2( t.x, 0.0), lod).rgb * 2.0;
    s += gBack.SampleLevel(gSamp, uv + float2(-t.x, 0.0), lod).rgb * 2.0;
    s += gBack.SampleLevel(gSamp, uv + float2(0.0,  t.y), lod).rgb * 2.0;
    s += gBack.SampleLevel(gSamp, uv + float2(0.0, -t.y), lod).rgb * 2.0;
    s += gBack.SampleLevel(gSamp, uv + t, lod).rgb;
    s += gBack.SampleLevel(gSamp, uv - t, lod).rgb;
    s += gBack.SampleLevel(gSamp, uv + float2(t.x, -t.y), lod).rgb;
    s += gBack.SampleLevel(gSamp, uv + float2(-t.x, t.y), lod).rgb;
    return s / 16.0;
}

float luma(float3 c) { return dot(c, float3(0.2126, 0.7152, 0.0722)); }

float hash(float2 p) { return frac(sin(dot(p, float2(12.9898, 78.233))) * 43758.5453); }

float4 ps(VSOut i) : SV_Target
{
    float2 px = i.pos.xy;
    float2 c = uShape.xy + uShape.zw * 0.5;
    float2 h = uShape.zw * 0.5;
    float d = sdRound(px, c, h, uRadius);
    float cov = saturate(0.5 - d);

    // Soft ambient shadow plus a tight contact shadow, faded out well before
    // the edge of the window so it never shows a hard border.
    float sd = max(sdRound(px - float2(0.0, 8.0 * uScale), c, h, uRadius), 0.0);
    float sigma = 13.0 * uScale;
    float shadow = exp(-sd * sd / (2.0 * sigma * sigma)) * lerp(0.20, 0.42, uDark);
    shadow += exp(-max(d, 0.0) / (2.0 * uScale)) * lerp(0.10, 0.22, uDark);
    float edge = min(min(px.x, px.y), min(uSize.x - px.x, uSize.y - px.y));
    shadow *= smoothstep(0.0, 14.0 * uScale, edge);
    shadow *= uOpacity * (1.0 - cov);

    if (cov <= 0.0)
        return float4(0.0, 0.0, 0.0, shadow);

    // Outward normal from the SDF gradient.
    float2 e1 = float2(1.0, 0.0);
    float2 n = float2(sdRound(px + e1.xy, c, h, uRadius) - sdRound(px - e1.xy, c, h, uRadius),
                      sdRound(px + e1.yx, c, h, uRadius) - sdRound(px - e1.yx, c, h, uRadius));
    n = n / max(length(n), 1e-4);

    float depth = max(-d, 0.0);
    float rimW = 18.0 * uScale;
    float t = saturate(1.0 - depth / rimW);
    float bend = t * t * t;

    float3 col;
    if (uBackdrop > 0.5)
    {
        // Refraction: near the rim, sample the world from beyond the edge.
        float2 sp = px + n * bend * uLens * uScale;
        col = frost(sp, uBlurLod);
        if (bend > 0.015)
        {
            float ca = bend * 3.0 * uScale;
            col.r = frost(sp + n * ca, uBlurLod).r;
            col.b = frost(sp - n * ca, uBlurLod).b;
        }

        // Vibrancy, then adapt to how bright the world is right behind this
        // pixel, so the text colour never has to change.
        col = lerp(luma(col).xxx, col, 1.3);
        float bg = luma(gBack.SampleLevel(gSamp, sp / uBackSize, uBlurLod + 2.0).rgb);
        if (uDark > 0.5)
            col *= lerp(0.80, 0.30, saturate((bg - 0.12) * 1.6));
        else
            col = lerp(col, float3(1.0, 1.0, 1.0), lerp(0.40, 0.78, saturate((0.70 - bg) * 1.4)));
        col = lerp(col, uTint.rgb, uTint.a);
    }
    else
    {
        col = uTint.rgb;
    }

    // Light: a crisp specular line on the rim that faces the light, a dimmer
    // bounce on the far side, and a faint glow inside the bevel.
    float facing = saturate(dot(n, uLight));
    float away = saturate(dot(n, -uLight));
    float line_ = 1.0 - smoothstep(0.0, 1.4 * uScale, abs(depth - 0.6 * uScale));
    float spec = line_ * (0.10 + 0.70 * facing * facing + 0.22 * away * away * away);
    col += spec * lerp(0.85, 0.55, uDark);
    col += bend * (0.035 + 0.09 * facing) * lerp(1.0, 0.8, uDark);
    if (uDark < 0.5)
    {
        // Light theme needs a hint of edge definition against white windows.
        float edge = 1.0 - smoothstep(0.0, 1.0 * uScale, depth);
        col = lerp(col, col * 0.72, edge * 0.45 * (1.0 - facing));
    }

    // Selection droplet: a small piece of thicker glass riding on the panel.
    if (uSelAlpha > 0.001)
    {
        float2 sc = uSel.xy + uSel.zw * 0.5;
        float ds = sdRound(px, sc, uSel.zw * 0.5, uSelRadius);
        float sCov = saturate(0.5 - ds) * uSelAlpha;
        float sDepth = max(-ds, 0.0);
        float sT = saturate(1.0 - sDepth / (10.0 * uScale));
        col = lerp(col, float3(1.0, 1.0, 1.0), sCov * lerp(0.42, 0.085, uDark));
        col = lerp(col, uAccent.rgb, sCov * 0.07);
        col += sCov * sT * sT * lerp(0.08, 0.05, uDark);
        float2 sn = float2(sdRound(px + e1.xy, sc, uSel.zw * 0.5, uSelRadius) - sdRound(px - e1.xy, sc, uSel.zw * 0.5, uSelRadius),
                           sdRound(px + e1.yx, sc, uSel.zw * 0.5, uSelRadius) - sdRound(px - e1.yx, sc, uSel.zw * 0.5, uSelRadius));
        sn = sn / max(length(sn), 1e-4);
        float sLine = (1.0 - smoothstep(0.0, 1.2 * uScale, abs(ds + 0.6 * uScale))) * uSelAlpha;
        col += sLine * (0.05 + 0.30 * saturate(dot(sn, uLight))) * lerp(0.9, 0.7, uDark);
        if (uDark < 0.5)
            col -= sLine * 0.06 * saturate(dot(sn, -uLight));
    }

    // Dither away banding in the blur.
    col += (hash(px) - 0.5) * (1.5 / 255.0);

    float a = cov * uOpacity;
    return float4(saturate(col) * a, a + shadow);
}
