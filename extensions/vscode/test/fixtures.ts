/** Fixtures taken verbatim from the reference resource tree, shared by the
 * checker tests and the snippet tests. */

/** `examples/client_integration/python/simple/res/scripts/entity_defs/alias.xml`. */
export const ALIAS_XML = `<root>
	<OBJECT_ID>			INT32		</OBJECT_ID>
	<BOOL>				UINT8		</BOOL>
	<ANGLE>				FLOAT		</ANGLE>
	<ITEM>				UINT32		</ITEM>
	<CONTROLLER_ID>		INT32		</CONTROLLER_ID>
	<EXPERIENCE>		INT32		</EXPERIENCE>
	<INFO>				UINT16		</INFO>
	<MISSION_STATS> ARRAY <of>	INFO		 	</of>	</MISSION_STATS>
	<STATS_MATRIX>  ARRAY <of>	MISSION_STATS 	</of>	</STATS_MATRIX>
	<TEAM> ARRAY <of>OBJECT_ID	</of>			</TEAM>
</root>`;
