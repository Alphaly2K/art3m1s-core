#ifndef ARTEMIS_DXBC_EFFECT_HPP
#define ARTEMIS_DXBC_EFFECT_HPP

#include "artemis_dxbc_effect.h"

#include <array>
#include <cstring>
#include <stdexcept>
#include <string_view>
#include <utility>
#include <vector>

namespace artemis {

class DxbcEffect {
public:
    explicit DxbcEffect(std::string_view artifact) {
        read(artifact, &header_, sizeof(header_));
        require((header_.magic == ART_DXBC_EFFECT_MAGIC || header_.magic == ART_DXBC_EFFECT_PAIR_MAGIC) &&
            header_.bytecode_bytes >= 4 && header_.bytecode_bytes <= ART_DXBC_MAX_BYTECODE &&
            header_.constant_bytes <= ART_DXBC_MAX_CONSTANTS && !(header_.constant_bytes & 15) &&
            header_.textures <= ART_DXBC_TEXTURE_SLOTS && header_.uniforms <= ART_DXBC_MAX_UNIFORMS);
        read(artifact, textures_.data(), header_.textures * sizeof(ArtDxbcTexture));
        uniforms_.resize(header_.uniforms);
        if (!uniforms_.empty()) {
            read(artifact, uniforms_.data(), uniforms_.size() * sizeof(ArtDxbcUniform));
        }
        bytecode_ = read_bytecode(artifact, header_.bytecode_bytes);
        if (header_.magic == ART_DXBC_EFFECT_PAIR_MAGIC) {
            uint32_t bytes = 0;
            read(artifact, &bytes, sizeof(bytes));
            vertex_bytecode_ = read_bytecode(artifact, bytes);
        }
        require(artifact.empty());

        uint32_t slots = 0;
        for (uint32_t i = 0; i < header_.textures; ++i) {
            const auto &texture = textures_[i];
            require(name_valid(texture.name) && texture.slot < ART_DXBC_TEXTURE_SLOTS && !(slots & (1u << texture.slot)));
            for (uint32_t j = 0; j < i; ++j) require(std::strcmp(texture.name, textures_[j].name) != 0);
            slots |= 1u << texture.slot;
        }
        for (size_t i = 0; i < uniforms_.size(); ++i) {
            const auto &field = uniforms_[i];
            require(name_valid(field.name) && !(field.offset & 3) && field.components >= 1 && field.components <= 4 &&
                uint64_t(field.offset) + field.components * 4 <= header_.constant_bytes);
            for (size_t j = 0; j < i; ++j) {
                const auto &other = uniforms_[j];
                require(std::strcmp(field.name, other.name) != 0 &&
                    (field.offset + field.components * 4 <= other.offset ||
                     other.offset + other.components * 4 <= field.offset));
            }
        }
    }

    std::pair<std::string_view, std::string_view> take_bytecode() {
        return {std::exchange(vertex_bytecode_, {}), std::exchange(bytecode_, {})};
    }
    uint32_t constant_bytes() const { return header_.constant_bytes; }
    uint32_t texture_count() const { return header_.textures; }

    int texture_slot(std::string_view name) const {
        for (uint32_t i = 0; i < header_.textures; ++i) {
            if (name == textures_[i].name) return static_cast<int>(textures_[i].slot);
        }
        return -1;
    }

    bool set_uniform(std::vector<char> &block, std::string_view name, const float *values, size_t count) const {
        for (const auto &field : uniforms_) {
            if (name != field.name) continue;
            if (!values || count != field.components || block.size() != constant_bytes()) return false;
            std::memcpy(block.data() + field.offset, values, count * sizeof(float));
            return true;
        }
        return false;
    }

private:
    static void require(bool valid) {
        if (!valid) throw std::runtime_error("invalid ADE shader effect");
    }

    static void read(std::string_view &artifact, void *output, size_t bytes) {
        require(bytes <= artifact.size());
        if (bytes) std::memcpy(output, artifact.data(), bytes);
        artifact.remove_prefix(bytes);
    }

    static std::string_view read_bytecode(std::string_view &artifact, uint32_t bytes) {
        require(bytes >= 4 && bytes <= ART_DXBC_MAX_BYTECODE && bytes <= artifact.size() &&
                std::memcmp(artifact.data(), "DXBC", 4) == 0);
        const auto code = artifact.substr(0, bytes);
        artifact.remove_prefix(bytes);
        return code;
    }

    static bool name_valid(const char (&name)[ART_DXBC_NAME_BYTES]) {
        return name[0] && std::memchr(name, 0, sizeof(name));
    }

    ArtDxbcHeader header_{};
    std::array<ArtDxbcTexture, ART_DXBC_TEXTURE_SLOTS> textures_{};
    std::vector<ArtDxbcUniform> uniforms_;
    std::string_view bytecode_, vertex_bytecode_;
};

}

#endif
