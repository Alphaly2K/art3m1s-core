#include <CoreVideo/CoreVideo.h>
#include "artemis_dxvk_pass.h"
#include "artemis_dxvk_device.hpp"
#include "artemis_dxbc_effect.hpp"

#include <algorithm>
#include <cstdio>

using dxvk::Com;

static void require(bool success, const char *message) {
    if (!success) throw std::runtime_error(message);
}

struct ArtDxvkContext {
    artemis::DxvkDevice host;
    Com<ID3D11RasterizerState> rasterizer;
    Com<ID3D11SamplerState> sampler;
    void *metal_device = host.metal_device();

    ArtDxvkContext() {
        auto *device = host.device();
        D3D11_RASTERIZER_DESC rasterizer_desc = {};
        rasterizer_desc.FillMode = D3D11_FILL_SOLID;
        rasterizer_desc.CullMode = D3D11_CULL_NONE;
        rasterizer_desc.DepthClipEnable = TRUE;
        require(SUCCEEDED(device->CreateRasterizerState(&rasterizer_desc, &rasterizer)), "Cannot create rasterizer");
        D3D11_SAMPLER_DESC sampler_desc = {};
        sampler_desc.Filter = D3D11_FILTER_MIN_MAG_MIP_POINT;
        sampler_desc.AddressU = sampler_desc.AddressV = sampler_desc.AddressW = D3D11_TEXTURE_ADDRESS_CLAMP;
        sampler_desc.MaxAnisotropy = 1;
        sampler_desc.ComparisonFunc = D3D11_COMPARISON_NEVER;
        require(SUCCEEDED(device->CreateSamplerState(&sampler_desc, &sampler)), "Cannot create sampler");
        auto *context = host.context();
        context->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
        context->RSSetState(rasterizer.ptr());
        ID3D11SamplerState *active_sampler = sampler.ptr();
        context->PSSetSamplers(0, 1, &active_sampler);
    }

    ~ArtDxvkContext() { host.context()->ClearState(); }
};

struct ArtDxvkProgram {
    ID3D11Device *owner;
    artemis::DxbcEffect effect;
    const int fore, mask, user;
    Com<ID3D11VertexShader> vertex;
    Com<ID3D11PixelShader> pixel;
    Com<ID3D11Buffer> constants;
    std::vector<char> data;

    ArtDxvkProgram(ID3D11Device *device, std::string_view artifact)
        : owner(device), effect(artifact), fore(effect.texture_slot("textureFore")),
          mask(effect.texture_slot("textureMask")), user(effect.texture_slot("textureUser")),
          data(effect.constant_bytes()) {
        require(effect.texture_count() == uint32_t((fore >= 0) + (mask >= 0) + (user >= 0)),
                "Unsupported DXVK texture binding");
        const auto [vs, ps] = effect.take_bytecode();
        require(!vs.empty(), "DXVK program requires an ADE2 shader pair");
        require(SUCCEEDED(device->CreateVertexShader(vs.data(), vs.size(), nullptr, &vertex)), "Cannot create vertex shader");
        require(SUCCEEDED(device->CreatePixelShader(ps.data(), ps.size(), nullptr, &pixel)), "Cannot create pixel shader");
        if (!data.empty()) {
            D3D11_BUFFER_DESC desc = {};
            desc.ByteWidth = effect.constant_bytes();
            desc.BindFlags = D3D11_BIND_CONSTANT_BUFFER;
            require(SUCCEEDED(device->CreateBuffer(&desc, nullptr, &constants)), "Cannot create shader constants");
        }
    }
};

struct ArtDxvkTargets {
    ID3D11Device *owner;
    std::unique_ptr<artemis::DxvkSurface> foreground, output;
    Com<ID3D11ShaderResourceView> foreground_view;
    Com<ID3D11RenderTargetView> target;
    D3D11_VIEWPORT viewport;

    ArtDxvkTargets(artemis::DxvkDevice &host, IOSurfaceRef source, IOSurfaceRef dest) : owner(host.device()) {
        require(source && dest && source != dest, "DXVK requires distinct foreground and output surfaces");
        const auto width = IOSurfaceGetWidth(dest), height = IOSurfaceGetHeight(dest);
        require(IOSurfaceGetWidth(source) == width && IOSurfaceGetHeight(source) == height,
                "DXVK surfaces have different sizes");
        foreground = artemis::DxvkSurface::import(host, source, D3D11_BIND_SHADER_RESOURCE);
        output = artemis::DxvkSurface::import(host, dest, D3D11_BIND_RENDER_TARGET);
        require(SUCCEEDED(owner->CreateShaderResourceView(foreground->texture(), nullptr, &foreground_view)),
                "Cannot create DXVK foreground view");
        require(SUCCEEDED(owner->CreateRenderTargetView(output->texture(), nullptr, &target)),
                "Cannot create DXVK output view");
        host.prepare_input(*foreground);
        viewport = {0, 0, float(width), float(height), 0, 1};
    }
};

static Com<ID3D11ShaderResourceView> upload(ID3D11Device *device, ArtDxvkImage image) {
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
    require(SUCCEEDED(device->CreateTexture2D(&desc, &data, &texture)), "Cannot upload shader texture");
    Com<ID3D11ShaderResourceView> view;
    require(SUCCEEDED(device->CreateShaderResourceView(texture.ptr(), nullptr, &view)), "Cannot create shader texture view");
    return view;
}

ArtDxvkContext *art_dxvk_context_create(void) try {
    return new ArtDxvkContext;
} catch (const std::exception &error) {
    std::fprintf(stderr, "%s\n", error.what());
    return nullptr;
}

void *art_dxvk_context_metal_device(const ArtDxvkContext *context) { return context->metal_device; }
void art_dxvk_context_destroy(ArtDxvkContext *context) { delete context; }

ArtDxvkProgram *art_dxvk_program_create(ArtDxvkContext *context, const uint8_t *ade, size_t size) try {
    require(context && ade && size, "DXVK requires compiled shader bytes");
    return new ArtDxvkProgram(context->host.device(), {reinterpret_cast<const char *>(ade), size});
} catch (const std::exception &error) {
    std::fprintf(stderr, "%s\n", error.what());
    return nullptr;
}

void art_dxvk_program_destroy(ArtDxvkProgram *program) { delete program; }

ArtDxvkTargets *art_dxvk_targets_create(ArtDxvkContext *context, IOSurfaceRef foreground, IOSurfaceRef output) try {
    require(context != nullptr, "DXVK requires a context");
    return new ArtDxvkTargets(context->host, foreground, output);
} catch (const std::exception &error) {
    std::fprintf(stderr, "%s\n", error.what());
    return nullptr;
}

void art_dxvk_targets_destroy(ArtDxvkTargets *targets) { delete targets; }

int art_dxvk_draw(ArtDxvkContext *context, ArtDxvkProgram *program, const ArtDxvkTargets *targets,
                  ArtDxvkImage mask, ArtDxvkImage user, const ArtDxvkUniform *uniforms, size_t count) try {
    require(context && program && targets && (uniforms || !count), "Invalid DXVK draw arguments");
    auto *device = context->host.device();
    require(program->owner == device && targets->owner == device, "DXVK draw uses objects from different devices");
    std::fill(program->data.begin(), program->data.end(), 0);
    for (size_t i = 0; i < count; ++i) {
        const auto &uniform = uniforms[i];
        require(uniform.name != nullptr, "DXVK uniform is missing a name");
        const std::string_view name(reinterpret_cast<const char *>(uniform.name), uniform.name_length);
        require(program->effect.set_uniform(program->data, name, uniform.values, uniform.count), "Invalid DXVK uniform binding");
    }
    constexpr uint8_t white[] = {255, 255, 255, 255}, transparent[] = {0, 0, 0, 0};
    std::array<ID3D11ShaderResourceView *, ART_DXBC_TEXTURE_SLOTS> resources{};
    Com<ID3D11ShaderResourceView> mask_view, user_view;
    if (program->fore >= 0) resources[program->fore] = targets->foreground_view.ptr();
    if (program->mask >= 0) {
        mask_view = upload(device, mask.rgba ? mask : ArtDxvkImage{white, 1, 1});
        resources[program->mask] = mask_view.ptr();
    }
    if (program->user >= 0) {
        user_view = upload(device, user.rgba ? user : ArtDxvkImage{transparent, 1, 1});
        resources[program->user] = user_view.ptr();
    }
    auto *draw = context->host.context();
    auto *buffer = program->constants.ptr();
    if (buffer) draw->UpdateSubresource(buffer, 0, nullptr, program->data.data(), 0, 0);
    draw->PSSetConstantBuffers(0, 1, &buffer);
    draw->VSSetShader(program->vertex.ptr(), nullptr, 0);
    draw->PSSetShader(program->pixel.ptr(), nullptr, 0);
    draw->RSSetViewports(1, &targets->viewport);
    ID3D11RenderTargetView *view = targets->target.ptr();
    draw->OMSetRenderTargets(1, &view, nullptr);
    draw->PSSetShaderResources(0, resources.size(), resources.data());
    draw->Draw(4, 0);
    resources.fill(nullptr);
    draw->PSSetShaderResources(0, resources.size(), resources.data());
    draw->OMSetRenderTargets(0, nullptr, nullptr);
    draw->VSSetShader(nullptr, nullptr, 0);
    draw->PSSetShader(nullptr, nullptr, 0);
    buffer = nullptr;
    draw->PSSetConstantBuffers(0, 1, &buffer);
    context->host.finish_output(*targets->output);
    return 0;
} catch (const std::exception &error) {
    std::fprintf(stderr, "%s\n", error.what());
    return -1;
}
