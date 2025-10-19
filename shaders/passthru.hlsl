Texture2D screenTexture : register(t0);
SamplerState texSampler : register(s0);

float4 main(float4 pos : SV_POSITION, float2 texCoord : TEXCOORD) : SV_Target {
    return screenTexture.Sample(texSampler, texCoord);
}
