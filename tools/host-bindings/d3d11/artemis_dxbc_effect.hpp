#ifndef ARTEMIS_DXBC_EFFECT_HPP
#define ARTEMIS_DXBC_EFFECT_HPP

#include "artemis_dxbc_effect.h"

#include <algorithm>
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
        first_buffer_bytes_ = header_.constant_bytes;
        if (header_.magic == ART_DXBC_EFFECT_PAIR_MAGIC) {
            uint32_t vertex_bytes = 0;
            read(artifact, &vertex_bytes, sizeof(vertex_bytes));
            vertex_bytecode_ = read_bytecode(artifact, vertex_bytes);
            if (!artifact.empty()) {
                const auto bytes = header_.constant_bytes;
                require(artifact.size() == bytes || artifact.size() == bytes + sizeof(uint32_t));
                defaults_.assign(artifact.begin(), artifact.begin() + bytes);
                artifact.remove_prefix(bytes);
                if (!artifact.empty()) {
                    read(artifact, &first_buffer_bytes_, sizeof(first_buffer_bytes_));
                    require(first_buffer_bytes_ < bytes && !(first_buffer_bytes_ & 15));
                }
            }
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
            const auto elements = art_dxbc_uniform_elements(field.components);
            const auto columns = art_dxbc_uniform_columns(field.components);
            const auto span = art_dxbc_uniform_span(field.components);
            require(name_valid(field.name) && !(field.offset & 3) && columns >= 1 && columns <= 4 &&
                (!elements || (header_.magic == ART_DXBC_EFFECT_PAIR_MAGIC && !(field.offset & 15))) &&
                uint64_t(field.offset) + span <= (field.offset < first_buffer_bytes_
                    ? first_buffer_bytes_ : header_.constant_bytes));
            for (size_t j = 0; j < i; ++j) {
                const auto &other = uniforms_[j];
                require(std::strcmp(field.name, other.name) != 0 &&
                    (field.offset + span <= other.offset ||
                     other.offset + art_dxbc_uniform_span(other.components) <= field.offset));
            }
        }
    }

    std::pair<std::string_view, std::string_view> take_bytecode() {
        return {std::exchange(vertex_bytecode_, {}), std::exchange(bytecode_, {})};
    }
    uint32_t constant_bytes() const { return header_.constant_bytes; }
    uint32_t constant_offset(unsigned slot) const { return slot == 0 ? 0 : first_buffer_bytes_; }
    uint32_t constant_size(unsigned slot) const {
        return slot == 0 ? first_buffer_bytes_ : header_.constant_bytes - first_buffer_bytes_;
    }
    uint32_t texture_count() const { return header_.textures; }

    int texture_slot(std::string_view name) const {
        for (uint32_t i = 0; i < header_.textures; ++i) {
            if (name == textures_[i].name) return static_cast<int>(textures_[i].slot);
        }
        return -1;
    }

    void reset_uniforms(std::vector<char> &block) const {
        if (defaults_.empty()) std::fill(block.begin(), block.end(), 0);
        else std::copy(defaults_.begin(), defaults_.end(), block.begin());
    }

    bool set_uniform(std::vector<char> &block, std::string_view name, const float *values, size_t count) const {
        for (const auto &field : uniforms_) {
            if (name != field.name) continue;
            const auto columns = art_dxbc_uniform_columns(field.components);
            const auto elements = art_dxbc_uniform_elements(field.components);
            if (!values || count != columns * (elements ? elements : 1) || block.size() != constant_bytes()) return false;
            for (uint32_t i = 0; i < (elements ? elements : 1); ++i) {
                std::memcpy(block.data() + field.offset + i * 16, values + i * columns, columns * sizeof(float));
            }
            return true;
        }
        // A declared effect parameter may have been optimized out by the compiler.
        return true;
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
    uint32_t first_buffer_bytes_ = 0;
    std::array<ArtDxbcTexture, ART_DXBC_TEXTURE_SLOTS> textures_{};
    std::vector<ArtDxbcUniform> uniforms_;
    std::vector<char> defaults_;
    std::string_view bytecode_, vertex_bytecode_;
};

}

#endif
