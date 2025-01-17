/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Computed types for CSS values related to backgrounds.

use properties::animated_properties::{Animatable, RepeatableListAnimatable};
use properties::longhands::background_size::computed_value::T as BackgroundSizeList;
use values::animated::{ToAnimatedValue, ToAnimatedZero};
use values::computed::length::LengthOrPercentageOrAuto;
use values::generics::background::BackgroundSize as GenericBackgroundSize;

/// A computed value for the `background-size` property.
pub type BackgroundSize = GenericBackgroundSize<LengthOrPercentageOrAuto>;

impl RepeatableListAnimatable for BackgroundSize {}

impl Animatable for BackgroundSize {
    fn add_weighted(&self, other: &Self, self_portion: f64, other_portion: f64) -> Result<Self, ()> {
        match (self, other) {
            (
                &GenericBackgroundSize::Explicit { width: self_width, height: self_height },
                &GenericBackgroundSize::Explicit { width: other_width, height: other_height },
            ) => {
                Ok(GenericBackgroundSize::Explicit {
                    width: self_width.add_weighted(&other_width, self_portion, other_portion)?,
                    height: self_height.add_weighted(&other_height, self_portion, other_portion)?,
                })
            }
            _ => Err(()),
        }
    }

    #[inline]
    fn compute_distance(&self, other: &Self) -> Result<f64, ()> {
        self.compute_squared_distance(other).map(|sd| sd.sqrt())
    }

    #[inline]
    fn compute_squared_distance(&self, other: &Self) -> Result<f64, ()> {
        match (self, other) {
            (
                &GenericBackgroundSize::Explicit { width: self_width, height: self_height },
                &GenericBackgroundSize::Explicit { width: other_width, height: other_height },
            ) => {
                Ok(
                    self_width.compute_squared_distance(&other_width)? +
                    self_height.compute_squared_distance(&other_height)?
                )
            }
            _ => Err(()),
        }
    }
}

impl ToAnimatedZero for BackgroundSize {
    #[inline]
    fn to_animated_zero(&self) -> Result<Self, ()> { Err(()) }
}

impl ToAnimatedValue for BackgroundSize {
    type AnimatedValue = Self;

    #[inline]
    fn to_animated_value(self) -> Self {
        self
    }

    #[inline]
    fn from_animated_value(animated: Self::AnimatedValue) -> Self {
        use app_units::Au;
        use values::computed::Percentage;
        let clamp_animated_value = |value: LengthOrPercentageOrAuto| -> LengthOrPercentageOrAuto {
            match value {
                LengthOrPercentageOrAuto::Length(len) => {
                    LengthOrPercentageOrAuto::Length(Au(::std::cmp::max(len.0, 0)))
                },
                LengthOrPercentageOrAuto::Percentage(percent) => {
                    LengthOrPercentageOrAuto::Percentage(Percentage(percent.0.max(0.)))
                },
                _ => value
            }
        };
        match animated {
            GenericBackgroundSize::Explicit { width, height } => {
                GenericBackgroundSize::Explicit {
                    width: clamp_animated_value(width),
                    height: clamp_animated_value(height)
                }
            },
            _ => animated
        }
    }
}

impl ToAnimatedValue for BackgroundSizeList {
    type AnimatedValue = Self;

    #[inline]
    fn to_animated_value(self) -> Self {
        self
    }

    #[inline]
    fn from_animated_value(animated: Self::AnimatedValue) -> Self {
        BackgroundSizeList(ToAnimatedValue::from_animated_value(animated.0))
    }
}
