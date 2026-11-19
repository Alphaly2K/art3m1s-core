#include <CoreVideo/CoreVideo.h>
#include "artemis_dxvk_pass.h"
#include "artemis_dxvk_device.hpp"
#include "artemis_dxbc_effect.hpp"

#include <cstdio>
#include <fstream>

using dxvk::Com;

static void require(bool success, const char *message) {
    if (!success) throw std::runtime_error(message);
}

struct ArtDxvkPass {
    artemis::DxbcEffect effect;
    artemis::DxvkDevice host;
    std::unique_ptr<artemis::DxvkSurface> foreground, output;
    Com<ID3D11ShaderResourceView> foreground_view;
    Com<ID3D11RenderTargetView> target;
    Com<ID3D11Buffer> constants;
    void *metal_device;

    ArtDxvkPass(std::istream &artifact, IOSurfaceRef source, IOSurfaceRef dest)
        : effect(artifact), metal_device(host.metal_device()) {
        const auto &vs = effect.vertex_bytecode();
        require(!vs.empty(), "DXVK pass requires an ADE2 shader pair");
        require(source && dest && source != dest, "DXVK pass requires distinct foreground and output surfaces");
        const auto width = IOSurfaceGetWidth(dest), height = IOSurfaceGetHeight(dest);
        require(IOSurfaceGetWidth(source) == width && IOSurfaceGetHeight(source) == height,
                "DXVK pass surfaces have different sizes");
        foreground = artemis::DxvkSurface::import(host, source, D3D11_BIND_SHADER_RESOURCE);
        output = artemis::DxvkSurface::import(host, dest, D3D11_BIND_RENDER_TARGET);
        host.prepare_input(*foreground);

        auto *device = host.device();
        auto *context = host.context();
        require(SUCCEEDED(device->CreateShaderResourceView(foreground->texture(), nullptr, &foreground_view)),
                "Cannot create DXVK foreground view");
        require(SUCCEEDED(device->CreateRenderTargetView(output->texture(), nullptr, &target)),
                "Cannot create DXVK output view");
        const auto &ps = effect.bytecode();
        Com<ID3D11VertexShader> vertex;
        Com<ID3D11PixelShader> pixel;
        require(SUCCEEDED(device->CreateVertexShader(vs.data(), vs.size(), nullptr, &vertex)), "Cannot create vertex shader");
        require(SUCCEEDED(device->CreatePixelShader(ps.data(), ps.size(), nullptr, &pixel)), "Cannot create pixel shader");
        D3D11_RASTERIZER_DESC rasterizer_desc = {};
        rasterizer_desc.FillMode = D3D11_FILL_SOLID;
        rasterizer_desc.CullMode = D3D11_CULL_NONE;
        rasterizer_desc.DepthClipEnable = TRUE;
        Com<ID3D11RasterizerState> rasterizer;
        require(SUCCEEDED(device->CreateRasterizerState(&rasterizer_desc, &rasterizer)), "Cannot create rasterizer");
        D3D11_SAMPLER_DESC sampler_desc = {};
        sampler_desc.Filter = D3D11_FILTER_MIN_MAG_MIP_POINT;
        sampler_desc.AddressU = sampler_desc.AddressV = sampler_desc.AddressW = D3D11_TEXTURE_ADDRESS_CLAMP;
        sampler_desc.MaxAnisotropy = 1;
        sampler_desc.ComparisonFunc = D3D11_COMPARISON_NEVER;
        Com<ID3D11SamplerState> sampler;
        require(SUCCEEDED(device->CreateSamplerState(&sampler_desc, &sampler)), "Cannot create sampler");
        if (effect.constant_bytes()) {
            D3D11_BUFFER_DESC desc = {};
            desc.ByteWidth = effect.constant_bytes();
            desc.BindFlags = D3D11_BIND_CONSTANT_BUFFER;
            require(SUCCEEDED(device->CreateBuffer(&desc, nullptr, &constants)), "Cannot create shader constants");
        }
        context->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
        context->VSSetShader(vertex.ptr(), nullptr, 0);
        context->PSSetShader(pixel.ptr(), nullptr, 0);
        context->RSSetState(rasterizer.ptr());
        const D3D11_VIEWPORT viewport = {0, 0, float(width), float(height), 0, 1};
        context->RSSetViewports(1, &viewport);
        ID3D11SamplerState *active_sampler = sampler.ptr();
        context->PSSetSamplers(0, 1, &active_sampler);
        ID3D11Buffer *buffer = constants.ptr();
        context->PSSetConstantBuffers(0, 1, &buffer);
    }

    ~ArtDxvkPass() { host.context()->ClearState(); }

    Com<ID3D11ShaderResourceView> upload(ArtDxvkImage image) const {
        require(image.rgba && image.width && image.height && image.width <= D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION &&
                image.height <= D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION, "Invalid DXVK RGBA image");
        D3D11_TEXTURE2D_DESC desc = {};
        desc.Width = image.width;
        desc.Height = image.height;
        desc.MipLevels = desc.ArraySize = desc.SampleDesc.Count = 1;
        desc.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
        desc.Usage = D3D11_USAGE_IMMUTABLE;
        desc.BindFlags = D3D11_BIND_SHADER_RESOURCE;
        const D3D11_SUBRESOURCE_DATA data = {image.rgba, image.width * 4, 0};
        Com<ID3D11Texture2D> texture;
        require(SUCCEEDED(host.device()->CreateTexture2D(&desc, &data, &texture)), "Cannot upload shader texture");
        Com<ID3D11ShaderResourceView> view;
        require(SUCCEEDED(host.device()->CreateShaderResourceView(texture.ptr(), nullptr, &view)), "Cannot create shader texture view");
        return view;
    }

    void draw(ArtDxvkImage mask, ArtDxvkImage user, const ArtDxvkUniform *uniforms, size_t count) {
        std::vector<char> data(effect.constant_bytes());
        for (size_t i = 0; i < count; ++i) {
            const auto &uniform = uniforms[i];
            const std::string_view name(reinterpret_cast<const char *>(uniform.name), uniform.name_length);
            require(effect.set_uniform(data, name, uniform.values, uniform.count), "Invalid DXVK uniform binding");
        }
        const int fore = effect.texture_slot("textureFore"), mask_slot = effect.texture_slot("textureMask"),
                  user_slot = effect.texture_slot("textureUser");
        require(effect.texture_count() == uint32_t((fore >= 0) + (mask_slot >= 0) + (user_slot >= 0)),
                "Unsupported DXVK texture binding");
        constexpr uint8_t white[] = {255, 255, 255, 255}, transparent[] = {0, 0, 0, 0};
        std::array<ID3D11ShaderResourceView *, ART_DXBC_TEXTURE_SLOTS> resources{};
        Com<ID3D11ShaderResourceView> mask_view, user_view;
        if (fore >= 0) resources[fore] = foreground_view.ptr();
        if (mask_slot >= 0) {
            mask_view = upload(mask.rgba ? mask : ArtDxvkImage{white, 1, 1});
            resources[mask_slot] = mask_view.ptr();
        }
        if (user_slot >= 0) {
            user_view = upload(user.rgba ? user : ArtDxvkImage{transparent, 1, 1});
            resources[user_slot] = user_view.ptr();
        }
        auto *context = host.context();
        if (constants) context->UpdateSubresource(constants.ptr(), 0, nullptr, data.data(), 0, 0);
        ID3D11RenderTargetView *view = target.ptr();
        context->OMSetRenderTargets(1, &view, nullptr);
        context->PSSetShaderResources(0, resources.size(), resources.data());
        context->Draw(4, 0);
        resources.fill(nullptr);
        context->PSSetShaderResources(0, resources.size(), resources.data());
        context->OMSetRenderTargets(0, nullptr, nullptr);
        host.finish_output(*output);
    }
};

ArtDxvkPass *art_dxvk_pass_create(const char *effect, IOSurfaceRef foreground, IOSurfaceRef output) try {
    std::ifstream file(effect, std::ios::binary);
    return new ArtDxvkPass(file, foreground, output);
} catch (const std::exception &error) {
    std::fprintf(stderr, "%s\n", error.what());
    return nullptr;
}

void *art_dxvk_pass_metal_device(const ArtDxvkPass *pass) { return pass->metal_device; }

int art_dxvk_pass_draw(ArtDxvkPass *pass, ArtDxvkImage mask, ArtDxvkImage user,
                       const ArtDxvkUniform *uniforms, size_t count) try {
    pass->draw(mask, user, uniforms, count);
    return 0;
} catch (const std::exception &error) {
    std::fprintf(stderr, "%s\n", error.what());
    return -1;
}

void art_dxvk_pass_destroy(ArtDxvkPass *pass) { delete pass; }
