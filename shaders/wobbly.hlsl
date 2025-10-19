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
