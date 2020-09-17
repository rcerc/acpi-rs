use core::{mem, ops::Deref, ptr::NonNull};

/// Describes a physical mapping created by `AcpiHandler::map_physical_region` and unmapped by
/// `AcpiHandler::unmap_physical_region`. The region mapped must be at least `size_of::<T>()`
/// bytes, but may be bigger.
pub struct PhysicalMapping<H, T>
where
    H: AcpiHandler,
{
    pub physical_start: usize,
    pub virtual_start: NonNull<T>,
    pub region_length: usize, // Can be equal or larger than size_of::<T>()
    pub mapped_length: usize, // Differs from `region_length` if padding is added for alignment
    /*
     * NOTE: we store an `Option<H>` here to make the implementation of `coerce_type` easier - if we can find a
     * better way, that would be better. Other than that, this should never be `None`, so is fine to unwrap.
     */
    handler: Option<H>,
}

impl<H, T> PhysicalMapping<H, T>
where
    H: AcpiHandler,
{
    pub fn new(
        physical_start: usize,
        virtual_start: NonNull<T>,
        region_length: usize,
        mapped_length: usize,
        handler: H,
    ) -> PhysicalMapping<H, T> {
        PhysicalMapping { physical_start, virtual_start, region_length, mapped_length, handler: Some(handler) }
    }

    pub(crate) unsafe fn coerce_type<N>(mut self) -> PhysicalMapping<H, N> {
        /*
         * Ideally, we'd like to assert something like `self.region_length >= mem::size_of::<N>()` here, but we
         * can't as some types are actually sometimes larger than their tables, and use mechanisms such as
         * `ExtendedField` to mediate access.
         */
        assert!((self.virtual_start.as_ptr() as usize) % mem::align_of::<N>() == 0);

        let result = PhysicalMapping {
            physical_start: self.physical_start,
            virtual_start: NonNull::new(self.virtual_start.as_ptr() as *mut N).unwrap(),
            region_length: self.region_length,
            mapped_length: self.mapped_length,
            handler: mem::replace(&mut self.handler, None),
        };
        mem::forget(self);
        result
    }
}

impl<H, T> Deref for PhysicalMapping<H, T>
where
    H: AcpiHandler,
{
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { self.virtual_start.as_ref() }
    }
}

impl<H, T> Drop for PhysicalMapping<H, T>
where
    H: AcpiHandler,
{
    fn drop(&mut self) {
        self.handler.as_ref().unwrap().unmap_physical_region(self)
    }
}

/// An implementation of this trait must be provided to allow `acpi` to access platform-specific
/// functionality, such as mapping regions of physical memory. You are free to implement these
/// however you please, as long as they conform to the documentation of each function. The handler is stored in
/// every `PhysicalMapping` so it's able to unmap itself when dropped, so this type needs to be something you can
/// clone/move about freely (e.g. a reference, wrapper over `Rc`, marker struct, etc.).
pub trait AcpiHandler: Sized {
    /// Given a physical address and a size, map a region of physical memory that contains `T` (note: the passed
    /// size may be larger than `size_of::<T>()`). The address is not neccessarily page-aligned, so the
    /// implementation may need to map more than `size` bytes. The virtual address the region is mapped to does not
    /// matter, as long as it is accessible to `acpi`.
    unsafe fn map_physical_region<T>(&self, physical_address: usize, size: usize) -> PhysicalMapping<Self, T>;

    /// Unmap the given physical mapping. This is called when a `PhysicalMapping` is dropped.
    fn unmap_physical_region<T>(&self, region: &PhysicalMapping<Self, T>);
}
