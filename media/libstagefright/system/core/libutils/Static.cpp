/*
 * Copyright (C) 2008 The Android Open Source Project
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

// All static variables go here, to control initialization and
// destruction order in the library.

#ifdef ANDROID
#include <nscore.h>
extern "C" {
/*
 * To work around http://code.google.com/p/android/issues/detail?id=23203
 * we don't link with the crt objects. In some configurations, this means
 * a lack of the __dso_handle symbol because it is defined there, and
 * depending on the android platform and ndk versions used, it may or may
 * not be defined in libc.so. In the latter case, we fail to link. Defining
 * it here as weak makes us provide the symbol when it's not provided by
 * the crt objects, making the change transparent for future NDKs that
 * would fix the original problem. On older NDKs, it is not a problem
 * either because the way __dso_handle was used was already broken (and
 * the custom linker works around it).
 */
  NS_EXPORT __attribute__((weak)) void *__dso_handle;
}
#endif

namespace stagefright {

// For String8.cpp
extern void initialize_string8();
extern void terminate_string8();

// For String16.cpp
extern void initialize_string16();
extern void terminate_string16();

class LibUtilsFirstStatics
{
public:
    LibUtilsFirstStatics()
    {
        initialize_string8();
        initialize_string16();
    }
    
    ~LibUtilsFirstStatics()
    {
        terminate_string16();
        terminate_string8();
    }
};

static LibUtilsFirstStatics gFirstStatics;
int gDarwinCantLoadAllObjects = 1;

}   // namespace stagefright
