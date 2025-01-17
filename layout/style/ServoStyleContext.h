/* -*- Mode: C++; tab-width: 8; indent-tabs-mode: nil; c-basic-offset: 2 -*- */
/* vim: set ts=8 sts=2 et sw=2 tw=80: */
/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

#ifndef mozilla_ServoStyleContext_h
#define mozilla_ServoStyleContext_h

#include "nsStyleContext.h"

namespace mozilla {

namespace dom {
class Element;
} // namespace dom

class ServoStyleContext final : public nsStyleContext
{
public:
  ServoStyleContext(nsPresContext* aPresContext,
                    nsIAtom* aPseudoTag,
                    CSSPseudoElementType aPseudoType,
                    ServoComputedDataForgotten aComputedValues);

  nsPresContext* PresContext() const { return mPresContext; }
  const ServoComputedData* ComputedData() const { return &mSource; }

  void AddRef() { Servo_StyleContext_AddRef(this); }
  void Release() { Servo_StyleContext_Release(this); }

  ServoStyleContext* GetStyleIfVisited() const
  {
    return ComputedData()->visited_style.mPtr;
  }

  bool IsLazilyCascadedPseudoElement() const
  {
    return IsPseudoElement() &&
           !nsCSSPseudoElements::IsEagerlyCascadedInServo(GetPseudoType());
  }

  ServoStyleContext* GetCachedInheritingAnonBoxStyle(nsIAtom* aAnonBox) const;

  void SetCachedInheritedAnonBoxStyle(nsIAtom* aAnonBox,
                                      ServoStyleContext* aStyle)
  {
    MOZ_ASSERT(!GetCachedInheritingAnonBoxStyle(aAnonBox));
    MOZ_ASSERT(!aStyle->mNextInheritingAnonBoxStyle);

    // NOTE(emilio): Since we use it to cache inheriting anon boxes in a linked
    // list, we can't use that cache if the style we're inheriting from is an
    // inheriting anon box itself, since otherwise our parent would mistakenly
    // think that the style we're caching inherits from it.
    //
    // See the documentation of mNextInheritingAnonBoxStyle.
    if (IsInheritingAnonBox()) {
      return;
    }

    mNextInheritingAnonBoxStyle.swap(aStyle->mNextInheritingAnonBoxStyle);
    mNextInheritingAnonBoxStyle = aStyle;
  }

  ServoStyleContext* GetCachedLazyPseudoStyle(CSSPseudoElementType aPseudo) const;

  void SetCachedLazyPseudoStyle(ServoStyleContext* aStyle)
  {
    MOZ_ASSERT(aStyle->GetPseudo() && !aStyle->IsAnonBox());
    MOZ_ASSERT(!GetCachedLazyPseudoStyle(aStyle->GetPseudoType()));
    MOZ_ASSERT(!aStyle->mNextLazyPseudoStyle);
    MOZ_ASSERT(!IsLazilyCascadedPseudoElement(), "lazy pseudos can't inherit lazy pseudos");
    MOZ_ASSERT(aStyle->IsLazilyCascadedPseudoElement());

    // Since we're caching lazy pseudo styles on the ComputedValues of the
    // originating element, we can assume that we either have the same
    // originating element, or that they were at least similar enough to share
    // the same ComputedValues, which means that they would match the same
    // pseudo rules. This allows us to avoid matching selectors and checking
    // the rule node before deciding to share.
    //
    // The one place this optimization breaks is with pseudo-elements that
    // support state (like :hover). So we just avoid sharing in those cases.
    if (nsCSSPseudoElements::PseudoElementSupportsUserActionState(aStyle->GetPseudoType())) {
      return;
    }

    mNextLazyPseudoStyle.swap(aStyle->mNextLazyPseudoStyle);
    mNextLazyPseudoStyle = aStyle;
  }

  /**
   * Makes this context match |aOther| in terms of which style structs have
   * been resolved.
   */
  inline void ResolveSameStructsAs(const ServoStyleContext* aOther);

private:
  nsPresContext* mPresContext;
  ServoComputedData mSource;

  // A linked-list cache of inheriting anon boxes inheriting from this style _if
  // the style isn't an inheriting anon-box_.
  //
  // Otherwise it represents the next entry in the cache of the parent style
  // context.
  RefPtr<ServoStyleContext> mNextInheritingAnonBoxStyle;

  // A linked-list cache of lazy pseudo styles inheriting from this style _if
  // the style isn't a lazy pseudo style itself_.
  //
  // Otherwise it represents the next entry in the cache of the parent style
  // context.
  //
  // Note that we store these separately from inheriting anonymous boxes so that
  // text nodes inheriting from lazy pseudo styles can share styles, which is
  // very important on some pages.
  RefPtr<ServoStyleContext> mNextLazyPseudoStyle;
};

} // namespace mozilla

#endif // mozilla_ServoStyleContext_h
