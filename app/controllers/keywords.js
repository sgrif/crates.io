import Ember from 'ember';
import PaginationMixin from 'cargo/mixins/pagination';

const { computed } = Ember;

export default Ember.Controller.extend(PaginationMixin, {
    applicationController: Ember.inject.controller('application'),
    queryParams: ['page', 'per_page', 'sort'],
    page: '1',
    per_page: 10,
    sort: 'crates',
    showSortBy: false,

    totalItems: computed('model', function() {
        return this.store.metadataFor('keyword').total;
    }),

    currentSortBy: computed('sort', function() {
        if (this.get('sort') === 'crates') {
            return '# Crates';
        } else {
            return 'Alphabetical';
        }
    }),

    actions: {
        toggleShowSortBy() {
            var opt = 'showSortBy';
            this.get('applicationController').resetDropdownOption(this, opt);
        },
    },
});
