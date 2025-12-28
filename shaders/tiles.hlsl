// Texture inputs
Texture2D SourceImage : register(t0);
Texture2D TileSpritesheet : register(t1);
SamplerState samplerState : register(s0);

// Constants
cbuffer Constants : register(b0)
{
    float2 SourceResolution;      // e.g. (1920, 1080)
    float2 TileSize;              // e.g. (8, 16) - size of each tile in pixels
    uint TilesPerRow;              // e.g. 16 - columns in your spritesheet
    uint TotalTiles;               // e.g. 95 - total number of tiles
    float2 SpritesheetResolution; // Total spritesheet size
};

// Precomputed tile brightnesses (compute once on CPU, pass as buffer)
StructuredBuffer<float> TileBrightness : register(t2);

float GetAverageBrightness(Texture2D tex, float2 topLeft, float2 size, float2 texResolution)
{
    float brightness = 0.0;
    int samples = 16; // Sample a grid within the tile
    
    for (int y = 0; y < samples; y++)
    {
        for (int x = 0; x < samples; x++)
        {
            float2 offset = float2(x, y) / float(samples - 1);
            float2 uv = (topLeft + offset * size) / texResolution;
            float3 color = tex.Sample(samplerState, uv).rgb;
            
            // Simple luminance calculation
            brightness += dot(color, float3(0.299, 0.587, 0.114));
        }
    }
    
    return brightness / (samples * samples);
}

uint FindBestTile(float targetBrightness)
{
    uint bestTile = 0;
    float bestDiff = 1000.0;
    
    for (uint i = 0; i < TotalTiles; i++)
    {
        float diff = abs(TileBrightness[i] - targetBrightness);
        if (diff < bestDiff)
        {
            bestDiff = diff;
            bestTile = i;
        }
    }
    
    return bestTile;
}

float4 main(float4 pos : SV_POSITION, float2 texCoord : TEXCOORD) : SV_Target
{
    // Determine which tile this pixel belongs to
    float2 pixelPos = texCoord * SourceResolution;
    int2 tileIndex = int2(pixelPos / TileSize);

    // Calculate the region in the source image for this tile
    float2 sourceTileTopLeft = float2(tileIndex) * TileSize;

    // Get average brightness of this tile region in source
    float sourceBrightness = GetAverageBrightness(
        SourceImage,
        sourceTileTopLeft,
        TileSize,
        SourceResolution
    );

    // Find best matching tile from spritesheet
    uint bestTile = (uint)FindBestTile(sourceBrightness);

    // Calculate position within the current tile (0-1 range)
    float2 posInTile = frac(pixelPos / TileSize);

    // Calculate UV coordinates for the matched tile in spritesheet
    uint tileCol = bestTile % TilesPerRow;
    uint tileRow = bestTile / TilesPerRow;
    float2 spriteTileTopLeft = float2(tileCol, tileRow) * TileSize;
    float2 spriteUV = (spriteTileTopLeft + posInTile * TileSize) / SpritesheetResolution;

    // Sample from the matched tile
    return TileSpritesheet.Sample(samplerState, spriteUV);
}
