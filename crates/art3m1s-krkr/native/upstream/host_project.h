#pragma once

#include "art3m1s_krkr.h"

bool Art3m1sKrkrIsProjectKey(const char* key);
void Art3m1sKrkrMountProject(const Art3m1sKrkrProjectHostV1& host, const char* archive_key);
void Art3m1sKrkrUnmountProject();
