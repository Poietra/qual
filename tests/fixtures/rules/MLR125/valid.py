from manim import *


class Good(Scene):
    def construct(self):
        # Container use: the Mobject groups drawable children.
        container = Mobject()
        container.add(Dot())
        self.add(container)
        # A drawable leaf is fine.
        self.add(Square())
        # Updater host: an invisible per-frame driver is a known pattern.
        driver = Mobject()
        driver.add_updater(lambda mob, dt: None)
        self.add(driver)
        # Group has its own class; only the bare base Mobject is judged.
        self.add(Group())
        # Not added to the scene at all.
        unused = Mobject()
        self.wait()
