// GENERATED CODE - DO NOT MODIFY BY HAND
// coverage:ignore-file
// ignore_for_file: type=lint
// ignore_for_file: unused_element, deprecated_member_use, deprecated_member_use_from_same_package, use_function_type_syntax_for_parameters, unnecessary_const, avoid_init_to_null, invalid_override_different_default_values_named, prefer_expression_function_bodies, annotate_overrides, invalid_annotation_target, unnecessary_question_mark

part of 'engine.dart';

// **************************************************************************
// FreezedGenerator
// **************************************************************************

// dart format off
T _$identity<T>(T value) => value;
/// @nodoc
mixin _$FfiValue {





@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is FfiValue);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
  return 'FfiValue()';
}


}

/// @nodoc
class $FfiValueCopyWith<$Res>  {
$FfiValueCopyWith(FfiValue _, $Res Function(FfiValue) __);
}


/// Adds pattern-matching-related methods to [FfiValue].
extension FfiValuePatterns on FfiValue {
/// A variant of `map` that fallback to returning `orElse`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeMap<TResult extends Object?>({TResult Function( FfiValue_Null value)?  null_,TResult Function( FfiValue_Integer value)?  integer,TResult Function( FfiValue_Real value)?  real,TResult Function( FfiValue_Text value)?  text,TResult Function( FfiValue_Blob value)?  blob,required TResult orElse(),}){
final _that = this;
switch (_that) {
case FfiValue_Null() when null_ != null:
return null_(_that);case FfiValue_Integer() when integer != null:
return integer(_that);case FfiValue_Real() when real != null:
return real(_that);case FfiValue_Text() when text != null:
return text(_that);case FfiValue_Blob() when blob != null:
return blob(_that);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// Callbacks receives the raw object, upcasted.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case final Subclass2 value:
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult map<TResult extends Object?>({required TResult Function( FfiValue_Null value)  null_,required TResult Function( FfiValue_Integer value)  integer,required TResult Function( FfiValue_Real value)  real,required TResult Function( FfiValue_Text value)  text,required TResult Function( FfiValue_Blob value)  blob,}){
final _that = this;
switch (_that) {
case FfiValue_Null():
return null_(_that);case FfiValue_Integer():
return integer(_that);case FfiValue_Real():
return real(_that);case FfiValue_Text():
return text(_that);case FfiValue_Blob():
return blob(_that);}
}
/// A variant of `map` that fallback to returning `null`.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case final Subclass value:
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? mapOrNull<TResult extends Object?>({TResult? Function( FfiValue_Null value)?  null_,TResult? Function( FfiValue_Integer value)?  integer,TResult? Function( FfiValue_Real value)?  real,TResult? Function( FfiValue_Text value)?  text,TResult? Function( FfiValue_Blob value)?  blob,}){
final _that = this;
switch (_that) {
case FfiValue_Null() when null_ != null:
return null_(_that);case FfiValue_Integer() when integer != null:
return integer(_that);case FfiValue_Real() when real != null:
return real(_that);case FfiValue_Text() when text != null:
return text(_that);case FfiValue_Blob() when blob != null:
return blob(_that);case _:
  return null;

}
}
/// A variant of `when` that fallback to an `orElse` callback.
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return orElse();
/// }
/// ```

@optionalTypeArgs TResult maybeWhen<TResult extends Object?>({TResult Function()?  null_,TResult Function( PlatformInt64 field0)?  integer,TResult Function( double field0)?  real,TResult Function( String field0)?  text,TResult Function( Uint8List field0)?  blob,required TResult orElse(),}) {final _that = this;
switch (_that) {
case FfiValue_Null() when null_ != null:
return null_();case FfiValue_Integer() when integer != null:
return integer(_that.field0);case FfiValue_Real() when real != null:
return real(_that.field0);case FfiValue_Text() when text != null:
return text(_that.field0);case FfiValue_Blob() when blob != null:
return blob(_that.field0);case _:
  return orElse();

}
}
/// A `switch`-like method, using callbacks.
///
/// As opposed to `map`, this offers destructuring.
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case Subclass2(:final field2):
///     return ...;
/// }
/// ```

@optionalTypeArgs TResult when<TResult extends Object?>({required TResult Function()  null_,required TResult Function( PlatformInt64 field0)  integer,required TResult Function( double field0)  real,required TResult Function( String field0)  text,required TResult Function( Uint8List field0)  blob,}) {final _that = this;
switch (_that) {
case FfiValue_Null():
return null_();case FfiValue_Integer():
return integer(_that.field0);case FfiValue_Real():
return real(_that.field0);case FfiValue_Text():
return text(_that.field0);case FfiValue_Blob():
return blob(_that.field0);}
}
/// A variant of `when` that fallback to returning `null`
///
/// It is equivalent to doing:
/// ```dart
/// switch (sealedClass) {
///   case Subclass(:final field):
///     return ...;
///   case _:
///     return null;
/// }
/// ```

@optionalTypeArgs TResult? whenOrNull<TResult extends Object?>({TResult? Function()?  null_,TResult? Function( PlatformInt64 field0)?  integer,TResult? Function( double field0)?  real,TResult? Function( String field0)?  text,TResult? Function( Uint8List field0)?  blob,}) {final _that = this;
switch (_that) {
case FfiValue_Null() when null_ != null:
return null_();case FfiValue_Integer() when integer != null:
return integer(_that.field0);case FfiValue_Real() when real != null:
return real(_that.field0);case FfiValue_Text() when text != null:
return text(_that.field0);case FfiValue_Blob() when blob != null:
return blob(_that.field0);case _:
  return null;

}
}

}

/// @nodoc


class FfiValue_Null extends FfiValue {
  const FfiValue_Null(): super._();
  






@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is FfiValue_Null);
}


@override
int get hashCode => runtimeType.hashCode;

@override
String toString() {
  return 'FfiValue.null_()';
}


}




/// @nodoc


class FfiValue_Integer extends FfiValue {
  const FfiValue_Integer(this.field0): super._();
  

 final  PlatformInt64 field0;

/// Create a copy of FfiValue
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$FfiValue_IntegerCopyWith<FfiValue_Integer> get copyWith => _$FfiValue_IntegerCopyWithImpl<FfiValue_Integer>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is FfiValue_Integer&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'FfiValue.integer(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $FfiValue_IntegerCopyWith<$Res> implements $FfiValueCopyWith<$Res> {
  factory $FfiValue_IntegerCopyWith(FfiValue_Integer value, $Res Function(FfiValue_Integer) _then) = _$FfiValue_IntegerCopyWithImpl;
@useResult
$Res call({
 PlatformInt64 field0
});




}
/// @nodoc
class _$FfiValue_IntegerCopyWithImpl<$Res>
    implements $FfiValue_IntegerCopyWith<$Res> {
  _$FfiValue_IntegerCopyWithImpl(this._self, this._then);

  final FfiValue_Integer _self;
  final $Res Function(FfiValue_Integer) _then;

/// Create a copy of FfiValue
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(FfiValue_Integer(
null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as PlatformInt64,
  ));
}


}

/// @nodoc


class FfiValue_Real extends FfiValue {
  const FfiValue_Real(this.field0): super._();
  

 final  double field0;

/// Create a copy of FfiValue
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$FfiValue_RealCopyWith<FfiValue_Real> get copyWith => _$FfiValue_RealCopyWithImpl<FfiValue_Real>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is FfiValue_Real&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'FfiValue.real(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $FfiValue_RealCopyWith<$Res> implements $FfiValueCopyWith<$Res> {
  factory $FfiValue_RealCopyWith(FfiValue_Real value, $Res Function(FfiValue_Real) _then) = _$FfiValue_RealCopyWithImpl;
@useResult
$Res call({
 double field0
});




}
/// @nodoc
class _$FfiValue_RealCopyWithImpl<$Res>
    implements $FfiValue_RealCopyWith<$Res> {
  _$FfiValue_RealCopyWithImpl(this._self, this._then);

  final FfiValue_Real _self;
  final $Res Function(FfiValue_Real) _then;

/// Create a copy of FfiValue
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(FfiValue_Real(
null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as double,
  ));
}


}

/// @nodoc


class FfiValue_Text extends FfiValue {
  const FfiValue_Text(this.field0): super._();
  

 final  String field0;

/// Create a copy of FfiValue
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$FfiValue_TextCopyWith<FfiValue_Text> get copyWith => _$FfiValue_TextCopyWithImpl<FfiValue_Text>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is FfiValue_Text&&(identical(other.field0, field0) || other.field0 == field0));
}


@override
int get hashCode => Object.hash(runtimeType,field0);

@override
String toString() {
  return 'FfiValue.text(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $FfiValue_TextCopyWith<$Res> implements $FfiValueCopyWith<$Res> {
  factory $FfiValue_TextCopyWith(FfiValue_Text value, $Res Function(FfiValue_Text) _then) = _$FfiValue_TextCopyWithImpl;
@useResult
$Res call({
 String field0
});




}
/// @nodoc
class _$FfiValue_TextCopyWithImpl<$Res>
    implements $FfiValue_TextCopyWith<$Res> {
  _$FfiValue_TextCopyWithImpl(this._self, this._then);

  final FfiValue_Text _self;
  final $Res Function(FfiValue_Text) _then;

/// Create a copy of FfiValue
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(FfiValue_Text(
null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as String,
  ));
}


}

/// @nodoc


class FfiValue_Blob extends FfiValue {
  const FfiValue_Blob(this.field0): super._();
  

 final  Uint8List field0;

/// Create a copy of FfiValue
/// with the given fields replaced by the non-null parameter values.
@JsonKey(includeFromJson: false, includeToJson: false)
@pragma('vm:prefer-inline')
$FfiValue_BlobCopyWith<FfiValue_Blob> get copyWith => _$FfiValue_BlobCopyWithImpl<FfiValue_Blob>(this, _$identity);



@override
bool operator ==(Object other) {
  return identical(this, other) || (other.runtimeType == runtimeType&&other is FfiValue_Blob&&const DeepCollectionEquality().equals(other.field0, field0));
}


@override
int get hashCode => Object.hash(runtimeType,const DeepCollectionEquality().hash(field0));

@override
String toString() {
  return 'FfiValue.blob(field0: $field0)';
}


}

/// @nodoc
abstract mixin class $FfiValue_BlobCopyWith<$Res> implements $FfiValueCopyWith<$Res> {
  factory $FfiValue_BlobCopyWith(FfiValue_Blob value, $Res Function(FfiValue_Blob) _then) = _$FfiValue_BlobCopyWithImpl;
@useResult
$Res call({
 Uint8List field0
});




}
/// @nodoc
class _$FfiValue_BlobCopyWithImpl<$Res>
    implements $FfiValue_BlobCopyWith<$Res> {
  _$FfiValue_BlobCopyWithImpl(this._self, this._then);

  final FfiValue_Blob _self;
  final $Res Function(FfiValue_Blob) _then;

/// Create a copy of FfiValue
/// with the given fields replaced by the non-null parameter values.
@pragma('vm:prefer-inline') $Res call({Object? field0 = null,}) {
  return _then(FfiValue_Blob(
null == field0 ? _self.field0 : field0 // ignore: cast_nullable_to_non_nullable
as Uint8List,
  ));
}


}

// dart format on
