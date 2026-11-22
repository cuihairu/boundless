//! `ConverterMap` port (`conversion/converter_map.hpp`): the registry the
//! pipeline indexes converters by `typeId_`.

use std::collections::HashMap;

use crate::converter_info::ConverterInfo;

/// `ConverterMap`: the id → converter info registry.
pub type ConverterMap = HashMap<u64, ConverterInfo>;
