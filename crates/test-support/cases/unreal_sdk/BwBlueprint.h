// The Unreal adapter's Blueprint-visible surface (BW-21-04-03).
//
// The component exposes the SDK entry points a Blueprint can call and one
// entity-event delegate a Blueprint can subscribe to.  A Blueprint graph
// compiles these in the Unreal Editor, so the editor-side compile is a
// recorded gap; the surface itself is checkable from the declarations below
// without an editor.

#pragma once

#include "CoreMinimal.h"
#include "Components/ActorComponent.h"
#include "BwBlueprint.generated.h"

/// One entity event a Blueprint can receive: the entity's id and the kind.
DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(
    FBwEntityEvent,
    int64,
    EntityId,
    const FString&,
    Kind);

UCLASS(ClassGroup = (Boundless), Blueprintable, meta = (BlueprintSpawnableComponent))
class BWSDK_API UBwSdkComponent : public UActorComponent
{
    GENERATED_BODY()

public:
    /// A Blueprint logs the SDK in.
    UFUNCTION(BlueprintCallable, Category = "Boundless")
    bool Login(const FString& User, const FString& Password);

    /// A Blueprint logs the SDK out.
    UFUNCTION(BlueprintCallable, Category = "Boundless")
    void Logout();

    /// A Blueprint issues an RPC and reads the correlation id.
    UFUNCTION(BlueprintCallable, Category = "Boundless")
    int64 Rpc(int64 EntityId, const FString& Method, const TArray<FString>& Parameters);

    /// The entity-event delegate a Blueprint subscribes to.
    UPROPERTY(BlueprintAssignable, Category = "Boundless")
    FBwEntityEvent OnEntityEvent;
};
