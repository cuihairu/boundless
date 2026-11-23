// The Unreal adapter's game-thread dispatch (BW-21-04-04).
//
// The SDK raises entity events from its worker threads; the component posts
// each one to the game thread, where it fires the Blueprint delegate.  When
// the component is destroyed it stops posting, so a destroyed component
// dispatches nothing afterward.

#pragma once

#include "CoreMinimal.h"
#include "BwBlueprint.h"

UCLASS()
class BWSDK_API UBwSdkComponent : public UActorComponent
{
    GENERATED_BODY()

public:
    /// Post one entity event to the game thread.  The SDK worker threads
    /// call this; after the component is destroyed it is a no-op.
    void DispatchEntityEvent(int64 EntityId, const FString& Kind);

    /// The game-thread delivery: fire the Blueprint delegate, guarded so a
    /// destroyed component dispatches nothing.
    void OnGameThread(int64 EntityId, const FString& Kind);

    /// The component was destroyed: stop dispatching from here on.
    virtual void OnComponentDestroyed(bool bDestroyingHierarchy) override;
};
