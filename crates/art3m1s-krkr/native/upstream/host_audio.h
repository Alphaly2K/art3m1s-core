#pragma once

#include <cstdint>

#include "art3m1s_krkr.h"

namespace art3m1s::krkr
{
void ResetAudioHost();
bool PollAudioCommand(Art3m1sKrkrAudioCommandV1* out_command);
int32_t SubmitAudioConsumed(const Art3m1sKrkrAudioConsumedV1* consumed);
} // namespace art3m1s::krkr
