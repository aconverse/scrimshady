Texture2D inputTexture : register(t0);
SamplerState samplerState : register(s0);

float4 main(float4 pos : SV_POSITION, float2 uv : TEXCOORD) : SV_TARGET
{
    // Get texture dimensions
    uint width, height;
    inputTexture.GetDimensions(width, height);
    float fheight = float(height);
    
    // Get brightness of current pixel
    float3 color = inputTexture.Sample(samplerState, uv).rgb;
    float brightness = dot(color, float3(0.299, 0.587, 0.114)) + (uv.y / (30 * fheight));
    
    // Count how many pixels in this column are darker than current pixel
    int darkerCount = 0;
    for (uint ypp = 0; ypp < height; ypp++)
    {
	float ytarget = float(float(ypp) / fheight);
        float2 sampleUV = float2(uv.x, ytarget);
        float3 sampleColor = inputTexture.Sample(samplerState, sampleUV).rgb;
        float sampleBrightness = dot(sampleColor, float3(0.299, 0.587, 0.114)) + (ytarget / (30 * fheight));
        
        if (sampleBrightness < brightness)
            darkerCount++;
    }
    
    // debug viz
    //return float4(darkerCount / fheight, darkerCount / fheight, darkerCount / fheight, 1.0);

    // Read from the sorted position
    float2 sortedUV = float2(uv.x, darkerCount / fheight);
    return inputTexture.Sample(samplerState, sortedUV);
}
