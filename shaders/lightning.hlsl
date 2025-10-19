Texture2D screenTexture : register(t0);
SamplerState texSampler : register(s0);
cbuffer TimeBuffer : register(b0) {
    float Time;
    float3 padding;
}

// Hash function for noise generation
float hash(float2 p) {
    return frac(sin(dot(p, float2(127.1, 311.7))) * 43758.5453);
}

// 2D noise function
float noise(float2 p) {
    float2 i = floor(p);
    float2 f = frac(p);
    float2 u = f * f * (3.0 - 2.0 * f);

    float a = hash(i);
    float b = hash(i + float2(1.0, 0.0));
    float c = hash(i + float2(0.0, 1.0));
    float d = hash(i + float2(1.0, 1.0));

    return lerp(lerp(a, b, u.x), lerp(c, d, u.x), u.y);
}

// Fractal noise for more complex patterns
float fbm(float2 p) {
    float value = 0.0;
    float amplitude = 0.5;
    float frequency = 1.0;

    for(int i = 0; i < 4; i++) {
        value += amplitude * noise(p * frequency);
        frequency *= 2.0;
        amplitude *= 0.5;
    }
    return value;
}

float4 main(float4 pos : SV_POSITION, float2 texCoord : TEXCOORD) : SV_Target {
    float4 color = screenTexture.Sample(texSampler, texCoord);

    // Get texture dimensions for edge detection
    uint width, height;
    screenTexture.GetDimensions(width, height);
    float2 texelSize = float2(1.0 / width, 1.0 / height);

    // Multi-radius edge detection to affect nearby pixels
    float maxEdgeStrength = 0.0;
    float horizSharpMax = 0.0;
    float vertSharpMax = 0.0;

    // Check multiple radii around this pixel
    for(float radius = 0.0; radius <= 8.0; radius += 1.0) {
        float2 offset = texelSize * radius;

        // Sample in 8 directions at this radius
        for(int angle = 0; angle < 8; angle++) {
            float a = angle * 3.14159 / 4.0;
            float2 dir = float2(cos(a), sin(a)) * offset;
            float2 samplePos = texCoord + dir;

            // Edge detection at this sample point
            float4 left = screenTexture.Sample(texSampler, samplePos + float2(-texelSize.x, 0.0));
            float4 right = screenTexture.Sample(texSampler, samplePos + float2(texelSize.x, 0.0));
            float4 up = screenTexture.Sample(texSampler, samplePos + float2(0.0, -texelSize.y));
            float4 down = screenTexture.Sample(texSampler, samplePos + float2(0.0, texelSize.y));

            float3 horizGrad = abs(right.rgb - left.rgb);
            float3 vertGrad = abs(down.rgb - up.rgb);

            float horizEdge = dot(horizGrad, float3(0.299, 0.587, 0.114));
            float vertEdge = dot(vertGrad, float3(0.299, 0.587, 0.114));

            // Lower threshold for more sensitivity
            float edgeThreshold = 0.1;
            float horizSharp = smoothstep(edgeThreshold, edgeThreshold + 0.1, horizEdge);
            float vertSharp = smoothstep(edgeThreshold, edgeThreshold + 0.1, vertEdge);

            // Distance falloff - lightning is stronger near the actual edge
            float distanceFalloff = 1.0 - (radius / 8.0);
            distanceFalloff = distanceFalloff * distanceFalloff;

            float edgeStr = max(horizSharp, vertSharp) * distanceFalloff;
            if(edgeStr > maxEdgeStrength) {
                maxEdgeStrength = edgeStr;
                horizSharpMax = horizSharp;
                vertSharpMax = vertSharp;
            }
        }
    }

    // Lightning effect near edges
    float3 lightning = float3(0.0, 0.0, 0.0);

    if(maxEdgeStrength > 0.01) {
        // Create animated lightning using noise - more aggressive parameters
        float2 noiseCoord = texCoord * 80.0 + Time * 3.0;

        // Generate main lightning bolt pattern with more variation
        float n1 = fbm(noiseCoord);
        float n2 = fbm(noiseCoord * 2.0 + float2(Time * 5.0, -Time * 4.0));
        float n3 = fbm(noiseCoord * 0.5 + Time * 1.5);

        // Create branching pattern
        float bolt = n1 * n2 * (0.5 + 0.5 * n3);
        bolt = pow(bolt, 2.0); // Less concentrated for more coverage

        // Add directional bias based on edge orientation
        float direction = 0.0;
        if(horizSharpMax > vertSharpMax) {
            // Horizontal edge - lightning arcs vertically
            float arc = texCoord.x * 50.0 + Time * 6.0 + n1 * 10.0;
            direction = abs(sin(arc)) * abs(cos(arc * 0.5 + n2 * 3.0));
        } else {
            // Vertical edge - lightning arcs horizontally
            float arc = texCoord.y * 50.0 + Time * 6.0 + n2 * 10.0;
            direction = abs(sin(arc)) * abs(cos(arc * 0.5 + n1 * 3.0));
        }

        // Combine bolt pattern with direction
        float lightningIntensity = bolt * direction * maxEdgeStrength * 3.0;

        // Create aggressive flickering effect with sparks
        float flicker1 = abs(sin(Time * 30.0 + n1 * 15.0));
        float flicker2 = abs(sin(Time * 45.0 + n2 * 20.0));
        float spark = step(0.85, n3); // Random bright sparks
        float flicker = 0.5 + 0.5 * (flicker1 * flicker2) + spark * 0.8;
        lightningIntensity *= flicker;

        // Add multiple intensity levels for variety
        float core = pow(lightningIntensity, 1.5);
        float midGlow = pow(lightningIntensity, 3.0);

        // Bright blue-white lightning with intense core
        lightning = float3(
            0.6 + core * 1.5,      // R - bright white core
            0.7 + core * 1.3,      // G - bright white-blue
            1.0 + midGlow * 0.5    // B - intense blue
        ) * lightningIntensity * 8.0; // Much stronger base intensity

        // Add expansive outer glow that pulses
        float glowPulse = 0.7 + 0.3 * sin(Time * 4.0);
        float outerGlow = maxEdgeStrength * glowPulse;
        lightning += float3(0.3, 0.5, 1.2) * outerGlow * 3.0;

        // Add random electrical discharge spots
        float discharge = step(0.9, noise(texCoord * 100.0 + Time * 10.0));
        lightning += float3(0.8, 0.9, 1.5) * discharge * maxEdgeStrength * 5.0;
    }

    // Blend original color with lightning
    return float4(color.rgb + lightning, color.a);
}
