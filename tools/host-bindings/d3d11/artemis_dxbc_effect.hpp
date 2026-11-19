#ifndef ARTEMIS_DXBC_EFFECT_HPP
#define ARTEMIS_DXBC_EFFECT_HPP

#include "artemis_dxbc_effect.h"

#include <array>
#include <cstring>
#include <istream>
#include <stdexcept>
#include <string_view>
#include <vector>

namespace artemis {

class DxbcEffect {
public:
    explicit DxbcEffect(std::istream &file) {
        require(file.read(reinterpret_cast<char *>(&header_), sizeof(header_)).good() &&
            header_.magic == ART_DXBC_EFFECT_MAGIC &&
            header_.bytecode_bytes >= 4 && header_.bytecode_bytes <= ART_DXBC_MAX_BYTECODE &&
            header_.constant_bytes <= ART_DXBC_MAX_CONSTANTS && !(header_.constant_bytes & 15) &&
            header_.textures <= ART_DXBC_TEXTURE_SLOTS && header_.uniforms <= ART_DXBC_MAX_UNIFORMS);
        require(file.read(reinterpret_cast<char *>(textures_.data()), header_.textures * sizeof(ArtDxbcTexture)).good());
        uniforms_.resize(header_.uniforms);
        if (!uniforms_.empty()) {
            require(file.read(reinterpret_cast<char *>(uniforms_.data()), uniforms_.size() * sizeof(ArtDxbcUniform)).good());
        }
        bytecode_.resize(header_.bytecode_bytes);
        require(file.read(bytecode_.data(), bytecode_.size()).good() &&
            std::memcmp(bytecode_.data(), "DXBC", 4) == 0 && file.peek() == std::istream::traits_type::eof());

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

    const std::vector<char> &bytecode() const { return bytecode_; }
    uint32_t constant_bytes() const { return header_.constant_bytes; }

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
        if (!valid) throw std::runtime_error("invalid ADE1 shader effect");
    }

    static bool name_valid(const char (&name)[ART_DXBC_NAME_BYTES]) {
        return name[0] && std::memchr(name, 0, sizeof(name));
    }

    ArtDxbcHeader header_{};
    std::array<ArtDxbcTexture, ART_DXBC_TEXTURE_SLOTS> textures_{};
    std::vector<ArtDxbcUniform> uniforms_;
    std::vector<char> bytecode_;
};

}

#endif
