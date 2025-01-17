/* -*- Mode: C++; tab-width: 8; indent-tabs-mode: nil; c-basic-offset: 2 -*- */
/* vim: set ts=8 sts=2 et sw=2 tw=80: */
/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

#ifndef nsWindowSizes_h
#define nsWindowSizes_h

#include "mozilla/Assertions.h"
#include "mozilla/PodOperations.h"
#include "mozilla/SizeOfState.h"

class nsTabSizes {
public:
  enum Kind {
      DOM,        // DOM stuff.
      Style,      // Style stuff.
      Other       // Everything else.
  };

  nsTabSizes() { mozilla::PodZero(this); }

  void add(Kind kind, size_t n)
  {
    switch (kind) {
      case DOM:   mDom   += n; break;
      case Style: mStyle += n; break;
      case Other: mOther += n; break;
      default:    MOZ_CRASH("bad nsTabSizes kind");
    }
  }

  size_t mDom;
  size_t mStyle;
  size_t mOther;
};

#define NS_ARENA_SIZES_FIELD(classname) mArena##classname

struct nsArenaSizes {
#define FOR_EACH_SIZE(macro) \
  macro(Other, mLineBoxes) \
  macro(Style, mRuleNodes) \
  macro(Style, mStyleContexts) \
  macro(Style, mStyleStructs)

  nsArenaSizes()
    :
      #define ZERO_SIZE(kind, mSize) mSize(0),
      FOR_EACH_SIZE(ZERO_SIZE)
      #undef ZERO_SIZE
      #define FRAME_ID(classname, ...) NS_ARENA_SIZES_FIELD(classname)(),
      #define ABSTRACT_FRAME_ID(...)
      #include "nsFrameIdList.h"
      #undef FRAME_ID
      #undef ABSTRACT_FRAME_ID
      dummy()
  {}

  void addToTabSizes(nsTabSizes *sizes) const
  {
    #define ADD_TO_TAB_SIZES(kind, mSize) sizes->add(nsTabSizes::kind, mSize);
    FOR_EACH_SIZE(ADD_TO_TAB_SIZES)
    #undef ADD_TO_TAB_SIZES
    #define FRAME_ID(classname, ...) \
      sizes->add(nsTabSizes::Other, NS_ARENA_SIZES_FIELD(classname));
    #define ABSTRACT_FRAME_ID(...)
    #include "nsFrameIdList.h"
    #undef FRAME_ID
    #undef ABSTRACT_FRAME_ID
  }

  size_t getTotalSize() const
  {
    size_t total = 0;
    #define ADD_TO_TOTAL_SIZE(kind, mSize) total += mSize;
    FOR_EACH_SIZE(ADD_TO_TOTAL_SIZE)
    #undef ADD_TO_TOTAL_SIZE
    #define FRAME_ID(classname, ...) \
      total += NS_ARENA_SIZES_FIELD(classname);
    #define ABSTRACT_FRAME_ID(...)
    #include "nsFrameIdList.h"
    #undef FRAME_ID
    #undef ABSTRACT_FRAME_ID
    return total;
  }

  #define DECL_SIZE(kind, mSize) size_t mSize;
  FOR_EACH_SIZE(DECL_SIZE)
  #undef DECL_SIZE
  #define FRAME_ID(classname, ...) size_t NS_ARENA_SIZES_FIELD(classname);
  #define ABSTRACT_FRAME_ID(...)
  #include "nsFrameIdList.h"
  #undef FRAME_ID
  #undef ABSTRACT_FRAME_ID
  int dummy;  // present just to absorb the trailing comma from FRAME_ID in the
              // constructor

#undef FOR_EACH_SIZE
};

class nsWindowSizes
{
#define FOR_EACH_SIZE(macro) \
  macro(DOM,   mDOMElementNodesSize) \
  macro(DOM,   mDOMTextNodesSize) \
  macro(DOM,   mDOMCDATANodesSize) \
  macro(DOM,   mDOMCommentNodesSize) \
  macro(DOM,   mDOMEventTargetsSize) \
  macro(DOM,   mDOMPerformanceUserEntries) \
  macro(DOM,   mDOMPerformanceResourceEntries) \
  macro(DOM,   mDOMOtherSize) \
  macro(Style, mStyleSheetsSize) \
  macro(Other, mLayoutPresShellSize) \
  macro(Style, mLayoutStyleSetsSize) \
  macro(Other, mLayoutTextRunsSize) \
  macro(Other, mLayoutPresContextSize) \
  macro(Other, mLayoutFramePropertiesSize) \
  macro(Other, mPropertyTablesSize) \

public:
  explicit nsWindowSizes(mozilla::SizeOfState& aState)
    :
      #define ZERO_SIZE(kind, mSize)  mSize(0),
      FOR_EACH_SIZE(ZERO_SIZE)
      #undef ZERO_SIZE
      mDOMEventTargetsCount(0),
      mDOMEventListenersCount(0),
      mArenaSizes(),
      mState(aState)
  {}

  void addToTabSizes(nsTabSizes *sizes) const {
    #define ADD_TO_TAB_SIZES(kind, mSize) sizes->add(nsTabSizes::kind, mSize);
    FOR_EACH_SIZE(ADD_TO_TAB_SIZES)
    #undef ADD_TO_TAB_SIZES
    mArenaSizes.addToTabSizes(sizes);
  }

  size_t getTotalSize() const
  {
    size_t total = 0;
    #define ADD_TO_TOTAL_SIZE(kind, mSize) total += mSize;
    FOR_EACH_SIZE(ADD_TO_TOTAL_SIZE)
    #undef ADD_TO_TOTAL_SIZE
    total += mArenaSizes.getTotalSize();
    return total;
  }

  #define DECL_SIZE(kind, mSize) size_t mSize;
  FOR_EACH_SIZE(DECL_SIZE);
  #undef DECL_SIZE

  uint32_t mDOMEventTargetsCount;
  uint32_t mDOMEventListenersCount;

  nsArenaSizes mArenaSizes;
  mozilla::SizeOfState& mState;

#undef FOR_EACH_SIZE
};

#endif // nsWindowSizes_h
