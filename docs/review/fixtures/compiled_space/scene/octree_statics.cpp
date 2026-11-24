// Harness-only TU: MSVC accepted odr-use of in-class static constants without
// out-of-line definitions; modern GCC requires them. Original headers untouched.
#include "math/loose_octree.hpp"
namespace BW {
const LooseOctree_DataTypes::NodeIndex LooseOctree_DataTypes::INVALID_NODE;
const LooseOctree_DataTypes::NodeIndex LooseOctree_DataTypes::ROOT_NODE;
const LooseOctree_DataTypes::NodeDataReference
	LooseOctree_DataTypes::INVALID_DATAREFERENCE;
const LooseOctree_DataTypes::NodeIndex LooseOctree_DataTypes::LEAF_REF_FLAG;
const LooseOctree_DataTypes::NodeIndex LooseOctree_DataTypes::INDEX_MASK;
}
